// Keychain and SSH agent state as the UI sees it.
//
// Wraps `ferry-creds` so `app.rs` deals in "is a secret saved?" and
// "which agent keys exist?" rather than in channels and replies. Nothing here
// blocks: every keychain operation is in flight somewhere else, and `drain`
// collects the answers when they arrive.

use std::sync::mpsc::Receiver;

use crate::model::bookmarks::Bookmarks;
use crate::model::connection::{Connection, ConnectionId};
use crate::secrets::Secret;
use crate::secrets::agent::{self, AgentError, AgentKey};
use crate::secrets::vault::{Reply, SecretRef, Vault};

pub struct Keychain {
    vault: Vault,
    inbox: Receiver<Reply>,
    /// `None` until the startup probe answers.
    availability: Option<Result<(), String>>,
    probed: std::cell::Cell<bool>,
    agent_keys: Result<Vec<AgentKey>, AgentError>,
    /// A connection we are fetching the secret for, on the way to opening it.
    opening: Option<ConnectionId>,
    /// The answer, waiting to be collected by the UI.
    ready: Option<(ConnectionId, Option<Secret>)>,
}

impl Keychain {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (vault, inbox) = Vault::new(wake);
        // Nothing touches the keychain or the agent until something needs
        // them: the probe opens a D-Bus connection with threads of its own,
        // which an app that is only showing its welcome screen has no use for.
        Self {
            vault,
            inbox,
            availability: None,
            probed: std::cell::Cell::new(false),
            opening: None,
            ready: None,
            agent_keys: Err(AgentError::NotRunning),
        }
    }

    /// Ask once whether a keychain exists, the first time it matters.
    fn probe(&self) {
        if !self.probed.replace(true) {
            self.vault.probe();
        }
    }

    /// A local Unix socket round trip, measured in microseconds, so it is
    /// fine on the UI thread. Called when the editor opens, not per frame.
    pub fn refresh_agent(&mut self) {
        self.probe();
        self.agent_keys = agent::identities();
    }

    pub fn agent_keys(&self) -> Result<&[AgentKey], &AgentError> {
        self.agent_keys.as_deref()
    }

    /// `None` while the probe is still running, or before anything asked.
    pub fn keychain_problem(&self) -> Option<&str> {
        match &self.availability {
            Some(Err(why)) => Some(why),
            _ => None,
        }
    }

    /// Begin opening a connection: fetch whatever secret it needs. Methods that
    /// need none are answered immediately, so the caller has one code path.
    pub fn request_open(&mut self, connection: &Connection) {
        self.probe();
        match SecretRef::for_connection(connection) {
            Some(reference) => {
                self.opening = Some(connection.id);
                self.ready = None;
                self.vault.load(reference);
            }
            None => {
                self.opening = None;
                self.ready = Some((connection.id, None));
            }
        }
    }

    /// Collect a finished `request_open`, once.
    pub fn take_ready(&mut self) -> Option<(ConnectionId, Option<Secret>)> {
        self.ready.take()
    }

    /// Ask whether a secret already exists, so the editor can say so without
    /// showing it.
    pub fn check_saved(&self, connection: &Connection) {
        self.probe();
        if let Some(reference) = SecretRef::for_connection(connection) {
            self.vault.load(reference);
        }
    }

    /// Store the typed secret, or clear it when the auth method no longer has
    /// one. Leaving a stale password behind after switching to agent auth would
    /// be a small but real leak.
    pub fn save(&self, connection: &Connection, typed: &str) {
        self.probe();
        match SecretRef::for_connection(connection) {
            Some(reference) if !typed.is_empty() => {
                self.vault.store(reference, Secret::new(typed));
            }
            // Nothing typed: leave whatever is already stored alone.
            Some(_) => {}
            None => self.forget(connection),
        }
    }

    /// Remove every secret this connection could own, whichever auth method it
    /// currently uses.
    pub fn forget(&self, connection: &Connection) {
        self.probe();
        for slot in [
            crate::secrets::Slot::Password,
            crate::secrets::Slot::Passphrase,
            crate::secrets::Slot::SecretAccessKey,
        ] {
            self.vault.delete(SecretRef::new(connection.id, slot));
        }
    }

    /// Apply whatever the background threads have finished. Returns a status
    /// line when something worth telling the user about happened.
    /// Also says whether anything arrived at all.
    pub fn drain(&mut self, state: &mut Bookmarks) -> (Option<String>, bool) {
        let mut status = None;
        let mut any = false;
        while let Ok(reply) = self.inbox.try_recv() {
            any = true;
            match reply {
                Reply::Availability(result) => {
                    if let Err(why) = &result {
                        tracing::warn!(%why, "keychain unavailable");
                        status = Some(why.clone());
                    }
                    self.availability = Some(result);
                }
                Reply::Loaded(reference, secret) => {
                    self.mark_stored(state, reference.connection, secret.is_some());
                    if self.opening == Some(reference.connection) {
                        self.opening = None;
                        self.ready = Some((reference.connection, secret));
                    }
                }
                Reply::Stored(reference) => {
                    self.mark_stored(state, reference.connection, true);
                }
                Reply::Deleted(_) => {}
                Reply::Failed(reference, why) => {
                    tracing::error!(%why, "keychain operation failed");
                    // A connect waiting on this secret must not hang; let it
                    // proceed and report the missing secret itself.
                    if self.opening == Some(reference.connection) {
                        self.opening = None;
                        self.ready = Some((reference.connection, None));
                    }
                    status = Some(why);
                }
            }
        }
        (status, any)
    }

    fn mark_stored(&self, state: &mut Bookmarks, connection: ConnectionId, stored: bool) {
        if let Some(draft) = &mut state.editor
            && draft.connection.id == connection
        {
            draft.secret_stored = stored;
        }
    }
}
