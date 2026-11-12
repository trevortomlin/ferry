// The background engine.
//
// One thread, one tokio runtime, and one task per open session. Everything the
// network touches lives behind this boundary; the UI thread only ever posts a
// `Command` and drains `Event`s, so no amount of latency can stall a frame.
//
// Cancellation is cooperative and lives in `Sink::push`: between chunks, the
// session task peeks at its own inbox, and if a newer request is waiting it
// abandons the current listing. That means navigating away from a 100k-entry
// directory stops within one chunk rather than after the whole thing has
// transferred, and it needs no task aborting, no shared state, and no locks.

pub mod walk;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::model::connection::Connection;
use crate::model::entries::Entries;
use crate::model::message::{Event, Generation, SessionId};
use crate::model::transfer::{Conflict, Direction, TransferId};
use crate::remote::{Chunks, Finished, Progress, Provider};
use crate::secrets::Credentials;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// UI thread → engine. Lives here rather than in `core` because it carries
/// resolved credentials, and `core` must not depend on `creds`.
pub enum Command {
    Open {
        session: SessionId,
        connection: Box<Connection>,
        credentials: Credentials,
    },
    List {
        session: SessionId,
        generation: Generation,
        path: String,
    },
    Close {
        session: SessionId,
    },
    /// Ask what is already at the destination, before moving anything.
    Probe {
        session: SessionId,
        transfer: TransferId,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
    },
    /// Move one file. The engine reports progress and the outcome.
    Transfer {
        session: SessionId,
        transfer: TransferId,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
        /// Byte offset to continue from. `0` starts fresh, and clears whatever
        /// an earlier interrupted attempt left behind.
        resume_from: u64,
    },
    /// Stop a running transfer at the next block boundary.
    CancelTransfer {
        transfer: TransferId,
    },
    /// Walk a folder and report the files inside it.
    Expand {
        session: SessionId,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
    },
    /// Create, rename, or delete. `refresh` is the directory to re-list after.
    Operate {
        session: SessionId,
        operation: Operation,
    },
}

#[derive(Debug)]
pub enum Operation {
    CreateDirectory {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Delete {
        paths: Vec<(String, bool)>,
    },
    /// Move each path into a directory, keeping its name.
    Move {
        paths: Vec<String>,
        into: String,
    },
}

/// What one session task is told to do.
enum Work {
    List {
        generation: Generation,
        path: String,
    },
    Probe {
        transfer: TransferId,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
    },
    Transfer {
        transfer: TransferId,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
        resume_from: u64,
    },
    Expand {
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
    },
    Operate {
        operation: Operation,
    },
}

/// Set when the UI asks for a transfer to stop. The provider checks it between
/// blocks, so a cancel takes effect within one 32 KiB read rather than at the
/// end of the file.
type CancelFlag = Arc<std::sync::atomic::AtomicBool>;

pub struct Engine {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    next_session: u32,
    /// The thread's half, until the first command starts it. Nothing is
    /// spawned while the app sits on its welcome screen.
    pending: Option<(UnboundedReceiver<Command>, Sender<Event>, Wake)>,
}

/// Cancellation flags, shared between the UI's `CancelTransfer` and the task
/// doing the work. Kept in one place so a cancel arriving before the transfer
/// starts is still honoured.
#[derive(Default)]
struct Cancellations {
    flags: std::sync::Mutex<HashMap<TransferId, CancelFlag>>,
}

impl Cancellations {
    fn flag_for(&self, transfer: TransferId) -> CancelFlag {
        let mut flags = self.flags.lock().expect("cancellation flags poisoned");
        Arc::clone(flags.entry(transfer).or_default())
    }

    fn cancel(&self, transfer: TransferId) {
        self.flag_for(transfer)
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn forget(&self, transfer: TransferId) {
        self.flags
            .lock()
            .expect("cancellation flags poisoned")
            .remove(&transfer);
    }
}

impl Engine {
    /// Sets up the channels. The thread and its runtime start on the first
    /// command, so an app that never connects never pays for them.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (commands, inbox) = unbounded_channel();
        let (events, outbox) = channel();
        Self {
            commands,
            events: outbox,
            next_session: 0,
            pending: Some((inbox, events, Arc::new(wake))),
        }
    }

    fn start(&mut self) {
        let Some((inbox, events, wake)) = self.pending.take() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("engine".to_owned())
            .spawn(move || run(inbox, events, wake));
        if let Err(error) = spawned {
            tracing::error!(%error, "could not start the engine thread");
        }
    }

    /// Session ids are handed out here so the UI can record which session a
    /// view belongs to before the connection has been established.
    pub fn new_session(&mut self) -> SessionId {
        self.next_session = self.next_session.wrapping_add(1);
        SessionId(self.next_session)
    }

    pub fn send(&mut self, command: Command) {
        self.start();
        if self.commands.send(command).is_err() {
            tracing::error!("the engine thread is gone");
        }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

type Wake = Arc<dyn Fn() + Send + Sync>;

fn run(mut commands: UnboundedReceiver<Command>, events: Sender<Event>, wake: Wake) {
    // Two workers is enough: the work is waiting on sockets, not computing.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("engine-io")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "could not build the async runtime");
            return;
        }
    };

    let cancellations = Arc::new(Cancellations::default());

    runtime.block_on(async move {
        let mut sessions: HashMap<SessionId, UnboundedSender<Work>> = HashMap::new();

        while let Some(command) = commands.recv().await {
            match command {
                Command::Open {
                    session,
                    connection,
                    credentials,
                } => {
                    let (work, inbox) = unbounded_channel();
                    sessions.insert(session, work);
                    tokio::spawn(session_task(
                        session,
                        *connection,
                        credentials,
                        inbox,
                        events.clone(),
                        Arc::clone(&wake),
                        Arc::clone(&cancellations),
                    ));
                }
                Command::List {
                    session,
                    generation,
                    path,
                } => {
                    if let Some(work) = sessions.get(&session) {
                        let _ = work.send(Work::List { generation, path });
                    }
                }
                Command::Close { session } => {
                    // Dropping the sender ends the session task, which drops
                    // the provider and closes the socket.
                    sessions.remove(&session);
                }
                Command::Probe {
                    session,
                    transfer,
                    direction,
                    remote,
                    local,
                } => {
                    if let Some(work) = sessions.get(&session) {
                        let _ = work.send(Work::Probe {
                            transfer,
                            direction,
                            remote,
                            local,
                        });
                    }
                }
                Command::Transfer {
                    session,
                    transfer,
                    direction,
                    remote,
                    local,
                    resume_from,
                } => {
                    if let Some(work) = sessions.get(&session) {
                        let _ = work.send(Work::Transfer {
                            transfer,
                            direction,
                            remote,
                            local,
                            resume_from,
                        });
                    }
                }
                Command::CancelTransfer { transfer } => cancellations.cancel(transfer),
                Command::Expand {
                    session,
                    direction,
                    remote,
                    local,
                } => {
                    if let Some(work) = sessions.get(&session) {
                        let _ = work.send(Work::Expand {
                            direction,
                            remote,
                            local,
                        });
                    }
                }
                Command::Operate { session, operation } => {
                    if let Some(work) = sessions.get(&session) {
                        let _ = work.send(Work::Operate { operation });
                    }
                }
            }
        }
    });
}

async fn session_task(
    session: SessionId,
    connection: Connection,
    credentials: Credentials,
    mut inbox: UnboundedReceiver<Work>,
    events: Sender<Event>,
    wake: Wake,
    cancellations: Arc<Cancellations>,
) {
    let emit = |event: Event| {
        if events.send(event).is_ok() {
            wake();
        }
    };

    let mut provider = match Provider::connect(&connection, credentials).await {
        Ok(provider) => provider,
        Err(error) => {
            emit(Event::OpenFailed {
                session,
                message: error.to_string(),
            });
            return;
        }
    };

    let home = provider.home().await.unwrap_or_else(|error| {
        tracing::warn!(%error, "could not resolve the home directory; falling back to /");
        "/".to_owned()
    });
    emit(Event::Opened {
        session,
        connection: connection.id,
        home,
    });

    // A command the sink pulled off the queue while abandoning a listing.
    let mut deferred: Option<Work> = None;

    loop {
        let work = match deferred.take() {
            Some(work) => work,
            None => match inbox.recv().await {
                Some(work) => work,
                None => break,
            },
        };

        match work {
            Work::Probe {
                transfer,
                direction,
                remote,
                local,
            } => {
                let conflict = probe(&mut provider, direction, &remote, &local).await;
                emit(Event::TransferProbed { transfer, conflict });
            }

            Work::Transfer {
                transfer,
                direction,
                remote,
                local,
                resume_from,
            } => {
                // Starting fresh must not silently continue from an earlier
                // attempt, so anything left over is cleared first.
                if direction == Direction::Download
                    && let Some(parent) = local.parent()
                    && let Err(error) = std::fs::create_dir_all(parent)
                {
                    emit(Event::TransferFailed {
                        transfer,
                        message: format!("Could not create {}: {error}", parent.display()),
                    });
                    cancellations.forget(transfer);
                    continue;
                }
                if resume_from == 0 {
                    match direction {
                        Direction::Upload => provider.discard_partial(&remote).await,
                        Direction::Download => {
                            let _ = std::fs::remove_file(partial_local(&local));
                        }
                    }
                }
                run_transfer(
                    &mut provider,
                    transfer,
                    direction,
                    &remote,
                    &local,
                    resume_from,
                    &events,
                    &wake,
                    &cancellations,
                )
                .await;
                cancellations.forget(transfer);
            }

            Work::Expand {
                direction,
                remote,
                local,
            } => {
                let send = |batch: crate::engine::walk::Batch| {
                    let event = match batch {
                        crate::engine::walk::Batch::Items(items) => Event::Expanded {
                            session,
                            items,
                            last: false,
                        },
                        crate::engine::walk::Batch::Done => Event::Expanded {
                            session,
                            items: Vec::new(),
                            last: true,
                        },
                    };
                    if events.send(event).is_ok() {
                        wake();
                        true
                    } else {
                        false
                    }
                };

                let outcome = match direction {
                    Direction::Download => {
                        crate::engine::walk::remote_tree(&mut provider, &remote, &local, send).await
                    }
                    Direction::Upload => {
                        crate::engine::walk::local_tree(&mut provider, &local, &remote, send).await
                    }
                };
                if let Err(error) = outcome {
                    emit(Event::ExpandFailed {
                        session,
                        message: error.to_string(),
                    });
                }
            }

            Work::Operate { operation } => {
                let outcome = match &operation {
                    Operation::CreateDirectory { path } => provider
                        .create_dir(path)
                        .await
                        .map(|()| format!("Created {}", crate::model::path::file_name(path))),
                    Operation::Rename { from, to } => provider
                        .rename(from, to)
                        .await
                        .map(|()| format!("Renamed to {}", crate::model::path::file_name(to))),
                    Operation::Move { paths, into } => {
                        let mut moved = 0usize;
                        let mut failure = None;
                        for from in paths {
                            let to =
                                crate::model::path::join(into, crate::model::path::file_name(from));
                            match provider.rename(from, &to).await {
                                Ok(()) => moved += 1,
                                Err(error) => {
                                    failure = Some(error);
                                    break;
                                }
                            }
                        }
                        match failure {
                            Some(error) => Err(error),
                            None => Ok(format!(
                                "Moved {} into {}",
                                match moved {
                                    1 => "1 item".to_owned(),
                                    n => format!("{n} items"),
                                },
                                crate::model::path::file_name(into)
                            )),
                        }
                    }
                    Operation::Delete { paths } => {
                        let mut removed = 0usize;
                        let mut failure = None;
                        for (path, is_dir) in paths {
                            match provider.remove(path, *is_dir).await {
                                Ok(()) => removed += 1,
                                Err(error) => {
                                    failure = Some(error);
                                    break;
                                }
                            }
                        }
                        match failure {
                            Some(error) => Err(error),
                            None => Ok(match removed {
                                1 => "Deleted 1 item".to_owned(),
                                n => format!("Deleted {n} items"),
                            }),
                        }
                    }
                };
                emit(match outcome {
                    Ok(message) => Event::OperationDone { session, message },
                    Err(error) => Event::OperationFailed {
                        session,
                        message: error.to_string(),
                    },
                });
            }

            Work::List { generation, path } => {
                let mut sink = Sink {
                    session,
                    generation,
                    events: &events,
                    wake: &wake,
                    inbox: &mut inbox,
                    deferred: &mut deferred,
                };
                if let Err(error) = provider.list(&path, &mut sink).await {
                    emit(Event::Failed {
                        session,
                        generation,
                        message: error.to_string(),
                    });
                }
            }
        }
    }

    emit(Event::Closed { session });
}

/// Where a partially downloaded file waits. Must match the provider's choice.
fn partial_local(local: &std::path::Path) -> std::path::PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(".ferry-part");
    local.with_file_name(name)
}

/// What is already sitting at the destination, and how far an earlier attempt
/// got.
async fn probe(
    provider: &mut Provider,
    direction: Direction,
    remote: &str,
    local: &std::path::Path,
) -> Conflict {
    match direction {
        Direction::Download => Conflict {
            existing: std::fs::metadata(local).ok().map(|m| m.len()),
            partial: std::fs::metadata(partial_local(local))
                .ok()
                .map(|m| m.len()),
            source: provider.size_of(remote).await.unwrap_or(0),
        },
        Direction::Upload => Conflict {
            existing: provider.size_of(remote).await.ok(),
            partial: provider.partial_size(remote).await,
            source: std::fs::metadata(local).map(|m| m.len()).unwrap_or(0),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_transfer(
    provider: &mut Provider,
    transfer: TransferId,
    direction: Direction,
    remote: &str,
    local: &std::path::Path,
    resume_from: u64,
    events: &Sender<Event>,
    wake: &Wake,
    cancellations: &Cancellations,
) {
    let emit = |event: Event| {
        if events.send(event).is_ok() {
            wake();
        }
    };

    // Knowing the size up front is what makes the progress bar meaningful, but
    // failing to learn it is not a reason to refuse the transfer.
    let total = match direction {
        Direction::Download => provider.size_of(remote).await.unwrap_or(0),
        Direction::Upload => std::fs::metadata(local).map(|m| m.len()).unwrap_or(0),
    };
    emit(Event::TransferStarted { transfer, total });

    let mut reporter = Reporter {
        transfer,
        done: 0,
        last_sent: std::time::Instant::now(),
        events,
        wake,
        cancel: cancellations.flag_for(transfer),
    };

    let outcome = match direction {
        Direction::Download => {
            provider
                .download(remote, local, resume_from, &mut reporter)
                .await
        }
        Direction::Upload => {
            provider
                .upload(local, remote, resume_from, &mut reporter)
                .await
        }
    };

    emit(match outcome {
        Ok(Finished::Complete) => Event::TransferDone { transfer },
        Ok(Finished::Cancelled) => Event::TransferCancelled { transfer },
        Err(error) => Event::TransferFailed {
            transfer,
            message: error.to_string(),
        },
    });
}

/// Turns byte counts into throttled progress events, and answers the provider's
/// question about whether to keep going.
struct Reporter<'a> {
    transfer: TransferId,
    done: u64,
    last_sent: std::time::Instant,
    events: &'a Sender<Event>,
    wake: &'a Wake,
    cancel: CancelFlag,
}

/// A fast local transfer moves a 32 KiB block thousands of times a second.
/// Sending an event for each would swamp the UI thread for no visible benefit,
/// so progress is reported at about this interval instead.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

impl Progress for Reporter<'_> {
    fn advance(&mut self, bytes: u64) -> bool {
        self.done += bytes;
        if self.last_sent.elapsed() >= PROGRESS_INTERVAL {
            self.last_sent = std::time::Instant::now();
            if self
                .events
                .send(Event::TransferProgress {
                    transfer: self.transfer,
                    done: self.done,
                })
                .is_ok()
            {
                (self.wake)();
            } else {
                // The UI is gone.
                return false;
            }
        }
        !self.cancel.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Forwards each chunk to the UI, and stops the listing the moment a newer
/// request turns up.
struct Sink<'a> {
    session: SessionId,
    generation: Generation,
    events: &'a Sender<Event>,
    wake: &'a Wake,
    inbox: &'a mut UnboundedReceiver<Work>,
    deferred: &'a mut Option<Work>,
}

impl Chunks for Sink<'_> {
    fn push(&mut self, entries: Entries, last: bool) -> bool {
        let delivered = self
            .events
            .send(Event::Listing {
                session: self.session,
                generation: self.generation,
                entries,
                last,
            })
            .is_ok();
        if !delivered {
            // The UI is gone.
            return false;
        }
        (self.wake)();

        if last {
            return true;
        }

        // A newer request means the user has moved on. Keep it for the loop to
        // pick up, and stop reading this directory.
        match self.inbox.try_recv() {
            Ok(work) => {
                *self.deferred = Some(work);
                false
            }
            Err(_) => true,
        }
    }
}
