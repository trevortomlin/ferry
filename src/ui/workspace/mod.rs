//! The window: bookmarks on the left, one tab per open connection, the file
//! list, the transfers drawer, and the dialogs and menus over them.
//!
//! This is the only place that decides *what happens*. The engine thread owns
//! every socket and answers with events; the keychain answers on its own
//! threads. Both wake the UI through a channel, and `pump` folds their answers
//! in. Nothing here blocks or awaits the network.

mod browser;
mod chrome;
mod dialogs;
#[cfg(feature = "snapshot")]
mod fake;
mod menu;
mod panels;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferry::model::bookmarks::Bookmarks;
use ferry::model::browser::{Browser, Move};
use ferry::model::connection::{Connection, ConnectionId, Protocol};
use ferry::model::message::{Event, SessionId};
use ferry::model::order::{Filter, SortKey};
use ferry::model::path;
use ferry::model::settings::{Settings, Theme};
use ferry::model::transfer::{Direction, Queue, Resolution, State, TransferId};
use ferry::secrets::Secret;
use futures::StreamExt;
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, MouseButton, NavigationDirection,
    PathPromptOptions, Pixels, Render, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, WindowAppearance, actions, div, prelude::*, px,
};

use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::{format, theme};
use ferry::engine::walk;
use ferry::engine::{Command, Engine, Operation};
use ferry::secrets::keychain::Keychain;

pub use browser::*;
pub use dialogs::Dialog;
pub use menu::Menu;

actions!(
    ferry,
    [
        NewConnection,
        CloseTab,
        NextTab,
        PrevTab,
        EditLocation,
        FocusFilter,
        ToggleHidden,
        ToggleTransfers,
        ShowHelp,
        ShowSettings,
        Quit,
        Dismiss,
        Download,
        DownloadTo,
        UploadFiles,
        UploadFolder,
        NewFolder,
        Rename,
        DeleteSelected,
        CopyUrl,
        DialogConfirm,
        DialogCancel,
        DialogNext,
        DialogPrev,
        InputConfirm,
        InputCancel,
        InputDown,
    ]
);

/// How long a finished transfer stays in the drawer. Failures stay until
/// dismissed.
const TRANSFER_LINGER: Duration = Duration::from_secs(60);

/// Where a tab is in its life.
#[derive(Clone, PartialEq)]
pub enum Phase {
    /// Waiting on the keychain. `asked` once the request is in flight; the
    /// keychain answers one connection at a time.
    Unlocking {
        asked: bool,
    },
    /// The engine is connecting.
    Connecting,
    Ready,
    /// Could not connect, or the user closed the password prompt.
    Failed(SharedString),
}

/// One open connection.
pub struct Tab {
    pub id: u64,
    pub connection: Connection,
    pub browser: Browser,
    pub phase: Phase,
    /// Where the server said we start, for `~` in the path bar.
    home: String,
    back: Vec<String>,
    forward: Vec<String>,
    pub scroll: UniformListScrollHandle,
    pub filter: String,
    /// Names to select once the listing in flight lands: what was just
    /// created or renamed, or the folder we just came up out of.
    select_after: Vec<String>,
    /// Set while trying to enter a symbolic link: if it turns out not to be a
    /// directory, go back and open it as a file instead.
    link_attempt: Option<(String, String)>,
    /// Type-ahead buffer and when it was last added to.
    typeahead: (String, Instant),
}

impl Tab {
    fn new(id: u64, connection: Connection) -> Self {
        Self {
            id,
            connection,
            browser: Browser::default(),
            phase: Phase::Unlocking { asked: false },
            home: "/".to_owned(),
            back: Vec::new(),
            forward: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            filter: String::new(),
            select_after: Vec::new(),
            link_attempt: None,
            typeahead: (String::new(), Instant::now()),
        }
    }

    pub fn title(&self) -> String {
        if self.connection.name.trim().is_empty() {
            self.connection.summary()
        } else {
            self.connection.name.clone()
        }
    }

    pub fn session(&self) -> Option<SessionId> {
        self.browser.session
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
    pub fn can_up(&self) -> bool {
        self.phase == Phase::Ready && path::parent(&self.browser.path).is_some()
    }

    /// The visible row showing `name`, if any.
    fn row_of(&self, name: &str) -> Option<usize> {
        let entries = self.browser.entries();
        self.browser
            .order()
            .iter()
            .position(|entry| entries.name(entry) == name)
    }

    /// A URL for a remote path, for "Copy URL".
    fn url_for(&self, remote: &str) -> String {
        let c = &self.connection;
        match c.protocol {
            Protocol::Sftp => {
                let user = if c.username.is_empty() {
                    String::new()
                } else {
                    format!("{}@", c.username)
                };
                let port = if c.port == 22 {
                    String::new()
                } else {
                    format!(":{}", c.port)
                };
                format!("sftp://{user}{}{port}{remote}", c.host)
            }
            Protocol::S3 => format!("s3://{}{remote}", c.bucket),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    text: SharedString,
    kind: ToastKind,
}

/// Rows being dragged inside the list, to move them into a folder.
pub struct DraggedRows {
    pub tab: u64,
    /// The row grabbed, by name; its folder is the tab's.
    pub name: SharedString,
    /// Whether the whole selection came along with it.
    pub selection: bool,
}

pub struct Workspace {
    focus: FocusHandle,
    pub(crate) list_focus: FocusHandle,
    dialog_focus: FocusHandle,
    state: Bookmarks,
    creds: Keychain,
    engine: Engine,
    pub(crate) settings: Settings,
    tabs: Vec<Tab>,
    active: usize,
    next_tab: u64,

    transfers: Queue,
    /// Which session runs each transfer.
    transfer_session: HashMap<TransferId, SessionId>,
    /// Transfers whose destination has been asked about already.
    probed: HashSet<TransferId>,
    /// Downloads to open with the system's default app when they land.
    open_after: HashSet<TransferId>,
    /// The blocked transfer being asked about.
    asking: Option<TransferId>,
    /// "Do the same for the rest" in the conflict dialog.
    conflict_all: bool,
    conflict_policy: Option<Resolution>,
    /// Folder walks still running.
    expanding: usize,
    /// Directories that uploads landed in, refreshed once the queue is idle.
    uploaded_into: HashSet<(SessionId, String)>,
    /// Sessions whose tab was closed while transfers still needed them.
    lingering: HashSet<SessionId>,

    menu: Option<Menu>,
    dialog: Option<Dialog>,
    toast: Option<Toast>,
    toast_task: Option<Task<()>>,
    filter_input: Entity<TextInput>,
    location_input: Entity<TextInput>,
    editing_location: bool,
    /// Measured each frame, for Page Up / Page Down.
    list_height: Rc<Cell<Pixels>>,
    scroll_drag: Rc<Cell<Option<Pixels>>>,
    title: String,
    window_size: (f32, f32),
    save_layout: Option<Task<()>>,
    probed_startup: bool,
    panels: panels::Panels,
    /// Something changed that the file list or window frame shows.
    dirty: bool,
    /// Only these panels need redrawing (transfer progress, streaming chunks).
    dirty_panels: Vec<panels::PanelKind>,
    trim_task: Option<Task<()>>,
    /// `FERRY_SCROLL_PROBE=N`: frames left to scroll, and CPU per frame.
    scroll_probe: Option<(usize, f64, Vec<f64>)>,
    /// Something to focus once it is on screen.
    pending_focus: Option<FocusHandle>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Background threads cannot touch the UI; they poke this channel and
        // a foreground task drains whatever has arrived.
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        let engine_wake = {
            let tx = wake_tx.clone();
            move || {
                let _ = tx.unbounded_send(());
            }
        };
        let keychain_wake = move || {
            let _ = wake_tx.unbounded_send(());
        };
        let pump = cx.spawn(async move |this, cx| {
            while wake_rx.next().await.is_some() {
                // Coalesce a burst of wakes into one pass.
                while let Ok(()) = wake_rx.try_recv() {}
                if this.update(cx, |this, cx| this.pump(cx)).is_err() {
                    break;
                }
            }
        });

        let filter_input = cx.new(|cx| TextInput::new("Filter", cx));
        let location_input = cx.new(|cx| TextInput::new("Go to path…", cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &filter_input,
            window,
            |this, input, _: &TextInputEvent, _, cx| {
                let text = input.read(cx).text().to_string();
                this.set_filter(text, cx);
            },
        ));
        subs.push(cx.observe_window_appearance(window, |this, window, cx| {
            this.apply_theme(window);
            cx.notify();
        }));

        let settings = Settings::load();
        let mut this = Self {
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            state: Bookmarks::load(),
            creds: Keychain::new(keychain_wake),
            engine: Engine::spawn(engine_wake),
            window_size: (settings.window.width, settings.window.height),
            settings,
            tabs: Vec::new(),
            active: 0,
            next_tab: 1,
            transfers: Queue::default(),
            transfer_session: HashMap::new(),
            probed: HashSet::new(),
            open_after: HashSet::new(),
            asking: None,
            conflict_all: false,
            conflict_policy: None,
            expanding: 0,
            uploaded_into: HashSet::new(),
            lingering: HashSet::new(),
            menu: None,
            dialog: None,
            toast: None,
            toast_task: None,
            filter_input,
            location_input,
            editing_location: false,
            list_height: Rc::new(Cell::new(px(400.))),
            scroll_drag: Rc::new(Cell::new(None)),
            title: String::new(),
            save_layout: None,
            probed_startup: false,
            panels: panels::Panels::new(cx),
            dirty: false,
            dirty_panels: Vec::new(),
            trim_task: None,
            scroll_probe: std::env::var("FERRY_SCROLL_PROBE")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(|n| (n, 0.0, Vec::new())),
            pending_focus: None,
            _tasks: vec![pump],
            _subs: subs,
        };
        this.apply_theme(window);
        window.focus(&this.focus, cx);
        this.auto_open(window, cx);
        // Startup builds shaders, font tables and the first frame's layout,
        // and frees most of it again. glibc keeps those pages; hand them back
        // once the window has settled.
        this.trim_soon(cx);
        this
    }

    /// `FERRY_OPEN=<name>` opens a saved connection at startup.
    fn auto_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wanted) = std::env::var_os("FERRY_OPEN") else {
            return;
        };
        let wanted = wanted.to_string_lossy().to_lowercase();
        let found = self
            .state
            .connections()
            .iter()
            .find(|c| c.name.to_lowercase() == wanted)
            .map(|c| c.id);
        match found {
            Some(id) => self.open_connection(id, window, cx),
            None => tracing::warn!(%wanted, "FERRY_OPEN names no saved connection"),
        }
    }

    /// Return freed heap memory to the system a moment from now, after a
    /// burst of temporary allocations (startup, a big listing, closing a tab).
    pub(crate) fn trim_soon(&mut self, cx: &mut Context<Self>) {
        self.trim_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            release_free_memory();
        }));
    }

    fn apply_theme(&mut self, window: &Window) {
        let dark = match self.settings.theme {
            Theme::Dark => true,
            Theme::Light => false,
            Theme::System => matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        };
        theme::set_dark(dark);
    }

    // ── Background answers ──────────────────────────────────────────────

    fn pump(&mut self, cx: &mut Context<Self>) {
        self.drain_engine(cx);
        let (status, any) = self.creds.drain(&mut self.state);
        if let Some(status) = status {
            self.show_toast(status, ToastKind::Error, cx);
        }
        if any {
            self.dirty = true;
            self.poll_credentials(cx);
        }
        // Progress ticks and mid-stream chunks touch only the drawer and the
        // status bar; rebuilding the file list for them would be waste.
        if std::mem::take(&mut self.dirty) {
            cx.notify();
        } else if !self.dirty_panels.is_empty() {
            let mut which = std::mem::take(&mut self.dirty_panels);
            which.sort_by_key(|k| *k as u8);
            which.dedup();
            self.panels.poke(&which, cx);
        }
        self.dirty_panels.clear();
    }

    pub(crate) fn tab_ix_for(&self, session: SessionId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.browser.session == Some(session))
    }

    fn tab_ix_by_id(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub(crate) fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// The active tab, if it is connected and showing a directory.
    fn ready_tab(&self) -> Option<&Tab> {
        self.tab().filter(|t| t.phase == Phase::Ready)
    }

    fn drain_engine(&mut self, cx: &mut Context<Self>) {
        let mut trim = false;
        while let Some(mut event) = self.engine.try_recv() {
            match &event {
                Event::TransferProgress { .. } => self
                    .dirty_panels
                    .extend([panels::PanelKind::Transfers, panels::PanelKind::Status]),
                // Decided once it is applied, below.
                Event::Listing { .. } => {}
                _ => self.dirty = true,
            }
            match &mut event {
                Event::Opened { session, home, .. } => {
                    let Some(ix) = self.tab_ix_for(*session) else {
                        continue;
                    };
                    let tab = &mut self.tabs[ix];
                    tab.phase = Phase::Ready;
                    tab.home = home.clone();
                    let start = Some(tab.connection.initial_path.trim().to_owned())
                        .filter(|p| !p.is_empty())
                        .unwrap_or_else(|| home.clone());
                    self.navigate(ix, start, false, cx);
                    continue;
                }
                Event::OpenFailed { session, message } => {
                    if let Some(ix) = self.tab_ix_for(*session) {
                        let tab = &mut self.tabs[ix];
                        tab.phase = Phase::Failed(std::mem::take(message).into());
                        tab.browser.session = None;
                        tab.browser.loading = false;
                    }
                    continue;
                }
                Event::Closed { .. } => continue,

                Event::Expanded {
                    session,
                    items,
                    last,
                } => {
                    let session = *session;
                    for item in items.drain(..) {
                        let id =
                            self.transfers
                                .add(item.direction, item.remote, item.local, item.size);
                        self.transfer_session.insert(id, session);
                    }
                    if *last {
                        self.expanding = self.expanding.saturating_sub(1);
                        if self.expanding == 0 && self.transfers.active() == 0 {
                            self.show_toast("That folder is empty", ToastKind::Info, cx);
                        }
                    }
                    self.pump_transfers();
                    continue;
                }
                Event::ExpandFailed { message, .. } => {
                    self.expanding = self.expanding.saturating_sub(1);
                    let message = std::mem::take(message);
                    self.show_toast(message, ToastKind::Error, cx);
                    continue;
                }
                Event::TransferProbed { transfer, conflict } => {
                    let (id, conflict) = (*transfer, *conflict);
                    let blocked = self.transfers.blocked(id, conflict);
                    let policy = if self.open_after.contains(&id) {
                        // A file opened for viewing just replaces the cached
                        // copy; asking would be noise.
                        Some(Resolution::Overwrite)
                    } else {
                        self.conflict_policy
                    };
                    if blocked && let Some(policy) = policy {
                        if self.transfers.resolve(id, policy).is_none() {
                            self.probed.remove(&id);
                        }
                        self.pump_transfers();
                    } else if !blocked {
                        self.pump_transfers();
                    }
                    continue;
                }
                Event::TransferStarted { transfer, total } => {
                    self.transfers.start(*transfer, *total);
                    continue;
                }
                Event::TransferProgress { transfer, done } => {
                    self.transfers.progress(*transfer, *done);
                    continue;
                }
                Event::TransferDone { transfer } => {
                    let id = *transfer;
                    self.transfers.finish(id, State::Done);
                    if let Some(t) = self.transfers.get(id) {
                        if self.open_after.remove(&id) {
                            cx.open_with_system(&t.local);
                        } else if t.direction == Direction::Upload
                            && let Some(session) = self.transfer_session.get(&id)
                            && let Some(parent) = path::parent(&t.remote)
                        {
                            self.uploaded_into.insert((*session, parent.to_owned()));
                        }
                    }
                    self.after_transfer(id);
                    continue;
                }
                Event::TransferCancelled { transfer } => {
                    let id = *transfer;
                    self.transfers.finish(id, State::Cancelled);
                    self.open_after.remove(&id);
                    self.after_transfer(id);
                    continue;
                }
                Event::TransferFailed { transfer, message } => {
                    let id = *transfer;
                    self.transfers
                        .finish(id, State::Failed(std::mem::take(message)));
                    self.open_after.remove(&id);
                    if !self.settings.show_transfers {
                        self.show_toast("A transfer failed — see Transfers", ToastKind::Error, cx);
                    }
                    self.after_transfer(id);
                    continue;
                }

                Event::OperationDone { session, message } => {
                    let session = *session;
                    let message = std::mem::take(message);
                    self.show_toast(message, ToastKind::Success, cx);
                    if let Some(ix) = self.tab_ix_for(session) {
                        self.refresh_tab(ix);
                    }
                    continue;
                }
                Event::OperationFailed { session, message } => {
                    if let Some(ix) = self.tab_ix_for(*session) {
                        self.tabs[ix].select_after.clear();
                    }
                    let message = std::mem::take(message);
                    self.show_toast(message, ToastKind::Error, cx);
                    continue;
                }
                Event::Listing { .. } | Event::Failed { .. } | Event::Cancelled { .. } => {}
            }

            // A listing, or a listing's failure: it belongs to one tab.
            let session = match &event {
                Event::Listing { session, .. }
                | Event::Failed { session, .. }
                | Event::Cancelled { session, .. } => *session,
                _ => continue,
            };
            let Some(ix) = self.tab_ix_for(session) else {
                continue;
            };
            let (sort, hidden) = (self.settings.sort, self.settings.show_hidden);
            let failed = matches!(event, Event::Failed { .. });
            let tab = &mut self.tabs[ix];
            let filter = std::mem::take(&mut tab.filter);
            let before = (tab.browser.visible(), tab.browser.loading);
            let changed = tab
                .browser
                .apply(&mut event, sort, Filter::new(&filter, hidden));
            tab.filter = filter;
            if !changed {
                continue;
            }
            if ix == self.active
                && (tab.browser.visible(), tab.browser.loading) == before
                && !failed
            {
                // A chunk was stored but the visible rows did not change:
                // only the "so far" count moved.
                self.dirty_panels.push(panels::PanelKind::Status);
            } else {
                self.dirty = true;
            }
            if failed && let Some((prior, link)) = tab.link_attempt.take() {
                // Not a directory after all: back to where we were, and open
                // the link as a file.
                tab.browser.error = None;
                self.navigate(ix, prior, false, cx);
                self.open_remote_file(ix, link, 0, cx);
                continue;
            }
            if !tab.browser.loading {
                tab.browser.shrink();
                if tab.browser.total() > 10_000 {
                    trim = true;
                }
                if let Some((prior, _)) = tab.link_attempt.take()
                    && tab.browser.error.is_none()
                {
                    tab.back.push(prior);
                    tab.forward.clear();
                }
                let wanted = std::mem::take(&mut tab.select_after);
                let mut first = true;
                for name in &wanted {
                    if let Some(row) = tab.row_of(name) {
                        tab.browser
                            .focus(row, if first { Move::Replace } else { Move::Toggle });
                        if first {
                            tab.scroll.scroll_to_item(row, ScrollStrategy::Center);
                        }
                        first = false;
                    }
                }
            }
        }
        if trim {
            self.trim_soon(cx);
        }
    }

    // ── Connecting ──────────────────────────────────────────────────────

    pub(crate) fn open_connection(
        &mut self,
        id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An open tab for this connection is switched to, not duplicated.
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.connection.id == id && !matches!(t.phase, Phase::Failed(_)))
        {
            self.switch_tab(ix, window, cx);
            return;
        }
        let Some(connection) = self.state.get(id).cloned() else {
            return;
        };
        // A failed tab for it is retried in place.
        let ix = match self.tabs.iter().position(|t| t.connection.id == id) {
            Some(ix) => {
                let tab = &mut self.tabs[ix];
                tab.connection = connection;
                tab.phase = Phase::Unlocking { asked: false };
                tab.browser = Browser::default();
                ix
            }
            None => {
                let tab = Tab::new(self.next_tab, connection);
                self.next_tab += 1;
                self.tabs.push(tab);
                self.tabs.len() - 1
            }
        };
        self.switch_tab(ix, window, cx);
        self.kick_unlock();
        self.poll_credentials(cx);
        cx.notify();
    }

    /// The keychain answers one request at a time; ask for the next tab
    /// waiting on it, if none is being asked about.
    fn kick_unlock(&mut self) {
        if self
            .tabs
            .iter()
            .any(|t| t.phase == Phase::Unlocking { asked: true })
        {
            return;
        }
        if let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|t| t.phase == Phase::Unlocking { asked: false })
        {
            tab.phase = Phase::Unlocking { asked: true };
            self.creds.request_open(&tab.connection);
        }
    }

    fn poll_credentials(&mut self, cx: &mut Context<Self>) {
        let Some((id, secret)) = self.creds.take_ready() else {
            return;
        };
        let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.connection.id == id && t.phase == Phase::Unlocking { asked: true })
        else {
            self.kick_unlock();
            return;
        };
        let connection = self.tabs[ix].connection.clone();
        match ferry::secrets::assemble(&connection, secret) {
            Ok(credentials) => self.start_session(ix, credentials),
            Err(ferry::secrets::Needs::Secret { prompt }) => {
                let tab = self.tabs[ix].id;
                self.tabs[ix].phase = Phase::Unlocking { asked: false };
                self.ask_secret(tab, prompt, cx);
            }
        }
        self.kick_unlock();
    }

    fn start_session(&mut self, ix: usize, credentials: ferry::secrets::Credentials) {
        let session = self.engine.new_session();
        let tab = &mut self.tabs[ix];
        tab.browser.session = Some(session);
        tab.phase = Phase::Connecting;
        self.engine.send(Command::Open {
            session,
            connection: Box::new(tab.connection.clone()),
            credentials,
        });
    }

    /// The secret typed into the prompt: maybe keep it, then connect.
    fn connect_with_secret(&mut self, tab_id: u64, typed: String, remember: bool) {
        let Some(ix) = self.tab_ix_by_id(tab_id) else {
            return;
        };
        let connection = self.tabs[ix].connection.clone();
        if remember {
            self.creds.save(&connection, &typed);
        }
        match ferry::secrets::assemble(&connection, Some(Secret::new(typed))) {
            Ok(credentials) => self.start_session(ix, credentials),
            Err(ferry::secrets::Needs::Secret { .. }) => {
                self.tabs[ix].phase = Phase::Failed("A password is required".into());
            }
        }
    }

    pub(crate) fn reconnect(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        if let Some(session) = tab.browser.session.take() {
            self.engine.send(Command::Close { session });
        }
        tab.phase = Phase::Failed("".into());
        let id = tab.connection.id;
        self.open_connection(id, window, cx);
    }

    // ── Tabs ────────────────────────────────────────────────────────────

    pub(crate) fn switch_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        self.editing_location = false;
        let filter = self.tabs[ix].filter.clone();
        self.filter_input
            .update(cx, |input, cx| input.set_text(filter, None, cx));
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        if let Some(session) = tab.browser.session {
            let busy = self.transfers.all().iter().any(|t| {
                !t.state.is_finished() && self.transfer_session.get(&t.id) == Some(&session)
            });
            if busy {
                // Transfers still need the connection; close it after them.
                self.lingering.insert(session);
            } else {
                self.engine.send(Command::Close { session });
            }
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if ix < self.active {
            self.active -= 1;
        }
        if !self.tabs.is_empty() {
            self.switch_tab(self.active, window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    // ── Navigation ──────────────────────────────────────────────────────

    fn navigate(&mut self, ix: usize, target: String, history: bool, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        let Some(session) = tab.browser.session else {
            return;
        };
        let target = path::normalize(&target);
        let previous = tab.browser.path.clone();
        if history && !previous.is_empty() && previous != target {
            tab.back.push(previous.clone());
            tab.forward.clear();
        }
        // Going up selects the folder we came out of.
        if path::parent(&previous) == Some(target.as_str()) {
            tab.select_after = vec![path::file_name(&previous).to_owned()];
        }
        if !tab.filter.is_empty() {
            tab.filter.clear();
            if ix == self.active {
                self.filter_input
                    .update(cx, |input, cx| input.set_text("", None, cx));
            }
        }
        let tab = &mut self.tabs[ix];
        let generation = tab.browser.navigate(target.clone());
        tab.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.engine.send(Command::List {
            session,
            generation,
            path: target,
        });
        cx.notify();
    }

    fn refresh_tab(&mut self, ix: usize) {
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        let Some(session) = tab.browser.session else {
            return;
        };
        if tab.phase != Phase::Ready {
            return;
        }
        // Keep what was selected selected across the reload.
        if tab.select_after.is_empty() {
            let entries = tab.browser.entries();
            tab.select_after = tab
                .browser
                .selection()
                .iter()
                .map(|entry| entries.name(entry).to_owned())
                .collect();
        }
        let generation = tab.browser.refresh();
        let path = tab.browser.path.clone();
        self.engine.send(Command::List {
            session,
            generation,
            path,
        });
    }

    pub(crate) fn go(&mut self, target: String, cx: &mut Context<Self>) {
        if self.ready_tab().is_some() {
            self.navigate(self.active, target, true, cx);
        }
    }

    pub(crate) fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self
            .ready_tab()
            .and_then(|t| path::parent(&t.browser.path))
            .map(str::to_owned)
        {
            self.go(parent, cx);
        }
    }

    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) {
        let ix = self.active;
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        if let Some(target) = tab.back.pop() {
            tab.forward.push(tab.browser.path.clone());
            self.navigate(ix, target, false, cx);
        }
    }

    pub(crate) fn go_forward(&mut self, cx: &mut Context<Self>) {
        let ix = self.active;
        let Some(tab) = self.tabs.get_mut(ix) else {
            return;
        };
        if let Some(target) = tab.forward.pop() {
            tab.back.push(tab.browser.path.clone());
            self.navigate(ix, target, false, cx);
        }
    }

    /// Enter a directory, or open a file, at a visible row.
    pub(crate) fn open_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let ix = self.active;
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let Some(remote) = tab.browser.path_of(row) else {
            return;
        };
        let entry = tab.browser.order().get(row);
        let kind = tab.browser.entries().kind(entry);
        let size = tab.browser.entries().size(entry);
        match kind {
            ferry::model::entries::Kind::Directory => self.go(remote, cx),
            ferry::model::entries::Kind::Symlink => {
                let prior = tab.browser.path.clone();
                self.tabs[ix].link_attempt = Some((prior, remote.clone()));
                self.navigate(ix, remote, false, cx);
            }
            ferry::model::entries::Kind::File => {
                self.open_remote_file(ix, remote, size, cx);
            }
        }
    }

    pub(crate) fn set_filter(&mut self, text: String, cx: &mut Context<Self>) {
        let (sort, hidden) = (self.settings.sort, self.settings.show_hidden);
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.filter == text {
            return;
        }
        tab.filter = text;
        tab.browser.reorder(sort, Filter::new(&tab.filter, hidden));
        tab.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Rebuild every tab's visible order from the current settings.
    fn reorder_all(&mut self) {
        let (sort, hidden) = (self.settings.sort, self.settings.show_hidden);
        for tab in &mut self.tabs {
            tab.browser.reorder(sort, Filter::new(&tab.filter, hidden));
        }
    }

    pub(crate) fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.settings.sort.toggle(key);
        self.reorder_all();
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.save() {
            tracing::error!(%error, "could not save settings");
            self.show_toast(
                format!("Could not save settings: {error}"),
                ToastKind::Error,
                cx,
            );
        }
    }

    // ── Selection ───────────────────────────────────────────────────────

    /// Paths and whether each is a directory, for the selection — or the
    /// cursor row when nothing is selected.
    fn chosen(&self) -> Vec<(String, bool)> {
        let Some(tab) = self.ready_tab() else {
            return Vec::new();
        };
        let selected = tab.browser.selected_rows();
        if !selected.is_empty() {
            return selected;
        }
        let row = tab.browser.cursor;
        tab.browser
            .path_of(row)
            .map(|p| vec![(p, tab.browser.is_dir(row))])
            .unwrap_or_default()
    }

    // ── Transfers ───────────────────────────────────────────────────────

    fn queue_download(&mut self, session: SessionId, remote: String, local: PathBuf, size: u64) {
        let id = self.transfers.add(Direction::Download, remote, local, size);
        self.transfer_session.insert(id, session);
    }

    pub(crate) fn download(&mut self, into: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let Some(session) = tab.session() else {
            return;
        };
        let chosen = self.chosen();
        if chosen.is_empty() {
            self.show_toast("Select something to download", ToastKind::Info, cx);
            return;
        }
        let sizes: HashMap<String, u64> = {
            let entries = tab.browser.entries();
            tab.browser
                .order()
                .iter()
                .map(|e| {
                    (
                        path::join(&tab.browser.path, entries.name(e)),
                        entries.size(e),
                    )
                })
                .filter(|(p, _)| chosen.iter().any(|(c, _)| c == p))
                .collect()
        };
        let directory = into.unwrap_or_else(|| self.settings.downloads());
        let (mut files, mut folders) = (0, 0);
        for (remote, is_dir) in chosen {
            if is_dir {
                folders += 1;
                self.expanding += 1;
                self.engine.send(Command::Expand {
                    session,
                    direction: Direction::Download,
                    local: walk::destination_for(&directory, &remote),
                    remote,
                });
                continue;
            }
            let local = directory.join(path::file_name(&remote));
            let size = sizes.get(&remote).copied().unwrap_or(0);
            self.queue_download(session, remote, local, size);
            files += 1;
        }
        self.reveal_transfers(cx);
        self.show_toast(
            describe_queued(files, folders, &directory),
            ToastKind::Info,
            cx,
        );
        self.pump_transfers();
    }

    pub(crate) fn download_to(&mut self, cx: &mut Context<Self>) {
        if self.chosen().is_empty() {
            self.show_toast("Select something to download", ToastKind::Info, cx);
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Download here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = picked.await
                && let Some(dir) = paths.pop()
            {
                this.update(cx, |this, cx| this.download(Some(dir), cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Download a file to the cache and open it with the default app.
    fn open_remote_file(&mut self, ix: usize, remote: String, size: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        let Some(session) = tab.session() else {
            return;
        };
        let local = cache_dir()
            .join(tab.connection.id.to_string())
            .join(remote.trim_start_matches('/'));
        let name = path::file_name(&remote).to_owned();
        let id = self.transfers.add(Direction::Download, remote, local, size);
        self.transfer_session.insert(id, session);
        self.open_after.insert(id);
        self.show_toast(format!("Opening “{name}”…"), ToastKind::Info, cx);
        self.pump_transfers();
    }

    pub(crate) fn upload_paths(
        &mut self,
        paths: Vec<PathBuf>,
        into: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.ready_tab() else {
            self.show_toast("Connect to a server first", ToastKind::Info, cx);
            return;
        };
        let Some(session) = tab.session() else {
            return;
        };
        let directory = into.unwrap_or_else(|| tab.browser.path.clone());
        let (mut files, mut folders) = (0, 0);
        for local in paths {
            let name = local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let remote = path::join(&directory, &name);
            if local.is_dir() {
                folders += 1;
                self.expanding += 1;
                self.engine.send(Command::Expand {
                    session,
                    direction: Direction::Upload,
                    remote,
                    local,
                });
                continue;
            }
            let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            let id = self.transfers.add(Direction::Upload, remote, local, size);
            self.transfer_session.insert(id, session);
            files += 1;
        }
        self.reveal_transfers(cx);
        self.show_toast(
            describe_queued_upload(files, folders, &directory),
            ToastKind::Info,
            cx,
        );
        self.pump_transfers();
    }

    pub(crate) fn pick_uploads(&mut self, folders: bool, cx: &mut Context<Self>) {
        if self.ready_tab().is_none() {
            self.show_toast("Connect to a server first", ToastKind::Info, cx);
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !folders,
            directories: folders,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && !paths.is_empty()
            {
                this.update(cx, |this, cx| this.upload_paths(paths, None, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Show the drawer when something is queued, unless the user closed it
    /// on purpose since the last time it was empty.
    fn reveal_transfers(&mut self, cx: &mut Context<Self>) {
        if !self.settings.show_transfers
            && self.transfers.all().iter().all(|t| t.state.is_finished())
        {
            self.settings.show_transfers = true;
            self.save_settings(cx);
        }
    }

    /// Move the queue along: ask about the next transfer's destination, or
    /// start one whose destination is settled.
    fn pump_transfers(&mut self) {
        loop {
            let Some(id) = self.transfers.next_queued() else {
                return;
            };
            let Some(transfer) = self.transfers.get(id) else {
                return;
            };
            let Some(&session) = self.transfer_session.get(&id) else {
                self.transfers
                    .finish(id, State::Failed("The connection was closed".to_owned()));
                continue;
            };
            let (direction, remote, local, resume_from) = (
                transfer.direction,
                transfer.remote.clone(),
                transfer.local.clone(),
                transfer.resume_from,
            );
            if self.probed.contains(&id) {
                // Marked running here, so a second pump cannot start it twice.
                self.transfers.start(id, 0);
                self.engine.send(Command::Transfer {
                    session,
                    transfer: id,
                    direction,
                    remote,
                    local,
                    resume_from,
                });
            } else {
                self.probed.insert(id);
                self.transfers.probing(id);
                self.engine.send(Command::Probe {
                    session,
                    transfer: id,
                    direction,
                    remote,
                    local,
                });
            }
            return;
        }
    }

    fn after_transfer(&mut self, _id: TransferId) {
        self.pump_transfers();
        let idle = self.transfers.active() == 0 && self.expanding == 0;
        if idle {
            self.conflict_policy = None;
            self.conflict_all = false;
            self.probed.clear();
            // Uploads changed directories someone may be looking at.
            for (session, dir) in std::mem::take(&mut self.uploaded_into) {
                if let Some(ix) = self.tab_ix_for(session)
                    && self.tabs[ix].browser.path == dir
                {
                    self.refresh_tab(ix);
                }
            }
        }
        // Close connections whose tab went away once nothing needs them.
        let still_needed: HashSet<SessionId> = self
            .transfers
            .all()
            .iter()
            .filter(|t| !t.state.is_finished())
            .filter_map(|t| self.transfer_session.get(&t.id).copied())
            .collect();
        let done: Vec<SessionId> = self
            .lingering
            .iter()
            .filter(|s| !still_needed.contains(s))
            .copied()
            .collect();
        for session in done {
            self.lingering.remove(&session);
            self.engine.send(Command::Close { session });
        }
    }

    pub(crate) fn cancel_transfer(&mut self, id: TransferId) {
        if self.transfers.request_cancel(id) {
            self.engine.send(Command::CancelTransfer { transfer: id });
        } else {
            self.open_after.remove(&id);
        }
        self.pump_transfers();
    }

    pub(crate) fn retry_transfer(&mut self, id: TransferId) {
        let Some(t) = self.transfers.get(id).cloned() else {
            return;
        };
        let Some(&session) = self.transfer_session.get(&id) else {
            return;
        };
        self.transfers.remove(id);
        let new = self.transfers.add(t.direction, t.remote, t.local, t.total);
        self.transfer_session.insert(new, session);
        self.pump_transfers();
    }

    pub(crate) fn resolve_conflict(&mut self, resolution: Resolution, cx: &mut Context<Self>) {
        if self.conflict_all {
            self.conflict_policy = Some(resolution);
        }
        let mut targets: Vec<TransferId> = self.asking.into_iter().collect();
        if self.conflict_all {
            targets.extend(self.transfers.blocked_ids());
        }
        targets.sort();
        targets.dedup();
        for id in targets {
            if self.transfers.resolve(id, resolution).is_none() {
                self.probed.remove(&id);
            }
        }
        self.asking = None;
        self.pump_transfers();
        cx.notify();
    }

    // ── File operations ─────────────────────────────────────────────────

    fn operate(&mut self, operation: Operation) {
        if let Some(session) = self.ready_tab().and_then(Tab::session) {
            self.engine.send(Command::Operate { session, operation });
        }
    }

    pub(crate) fn create_folder(&mut self, name: String) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let path = path::join(&tab.browser.path, &name);
        tab.select_after = vec![name];
        self.operate(Operation::CreateDirectory { path });
    }

    pub(crate) fn rename_to(&mut self, from: String, name: String) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let to = path::join(path::parent(&from).unwrap_or("/"), &name);
        tab.select_after = vec![name];
        self.operate(Operation::Rename { from, to });
    }

    pub(crate) fn delete_paths(&mut self, paths: Vec<(String, bool)>) {
        if let Some(tab) = self.tab_mut() {
            tab.select_after.clear();
            let n = tab.browser.cursor;
            // Keep the cursor near where the deleted rows were.
            if let Some(next) = tab
                .browser
                .order()
                .iter()
                .skip(n)
                .map(|e| tab.browser.entries().name(e).to_owned())
                .find(|name| !paths.iter().any(|(p, _)| path::file_name(p) == name))
            {
                tab.select_after = vec![next];
            }
        }
        self.operate(Operation::Delete { paths });
    }

    pub(crate) fn move_into(&mut self, paths: Vec<String>, into: String, cx: &mut Context<Self>) {
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|p| p != &into && path::parent(p) != Some(into.as_str()))
            // Never into itself or below itself.
            .filter(|p| !into.starts_with(&format!("{p}/")))
            .collect();
        if paths.is_empty() {
            return;
        }
        if let Some(tab) = self.tab_mut() {
            tab.select_after.clear();
        }
        self.operate(Operation::Move { paths, into });
        cx.notify();
    }

    /// What a drag of rows carries, resolved against the tab it came from.
    pub(crate) fn dragged_paths(&self, d: &DraggedRows) -> Option<Vec<String>> {
        let tab = self.tab().filter(|t| t.id == d.tab)?;
        Some(if d.selection {
            tab.browser.selected_paths()
        } else {
            vec![path::join(&tab.browser.path, &d.name)]
        })
    }

    pub(crate) fn copy_url(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let chosen = self.chosen();
        let urls: Vec<String> = if chosen.is_empty() {
            vec![tab.url_for(&tab.browser.path)]
        } else {
            chosen.iter().map(|(p, _)| tab.url_for(p)).collect()
        };
        let text = urls.join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_toast(
            if urls.len() == 1 {
                "Copied the URL".to_owned()
            } else {
                format!("Copied {} URLs", urls.len())
            },
            ToastKind::Success,
            cx,
        );
    }

    // ── Toasts ──────────────────────────────────────────────────────────

    pub(crate) fn show_toast(
        &mut self,
        text: impl Into<SharedString>,
        kind: ToastKind,
        cx: &mut Context<Self>,
    ) {
        self.toast = Some(Toast {
            text: text.into(),
            kind,
        });
        let secs = if kind == ToastKind::Error { 8 } else { 4 };
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ── Actions ─────────────────────────────────────────────────────────

    fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    fn new_connection(&mut self, _: &NewConnection, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_connection(None, Protocol::Sftp, window, cx);
    }

    fn close_tab_action(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.close_tab(self.active, window, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.switch_tab((self.active + 1) % self.tabs.len(), window, cx);
        }
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            let n = self.tabs.len();
            self.switch_tab((self.active + n - 1) % n, window, cx);
        }
    }

    fn edit_location(&mut self, _: &EditLocation, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let text = tab.browser.path.clone();
        let len = text.len();
        self.location_input
            .update(cx, |input, cx| input.set_text(text, Some(0..len), cx));
        self.editing_location = true;
        let focus = self.location_input.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        if self.ready_tab().is_none() {
            return;
        }
        let focus = self.filter_input.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn input_confirm(&mut self, _: &InputConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_location {
            let text = self.location_input.read(cx).text().trim().to_owned();
            self.editing_location = false;
            if !text.is_empty() {
                let base = self
                    .ready_tab()
                    .map(|t| t.browser.path.clone())
                    .unwrap_or_else(|| "/".into());
                let target = if let Some(rest) = text.strip_prefix("~") {
                    format!("{}/{}", self.home_of_active(), rest.trim_start_matches('/'))
                } else {
                    path::join(&base, &text)
                };
                self.go(target, cx);
            }
        } else if let Some(tab) = self.ready_tab()
            && tab.browser.visible() > 0
        {
            // Enter in the filter: open the first match.
            let row = tab.browser.cursor.min(tab.browser.visible() - 1);
            self.open_row(row, cx);
        }
        self.focus_list(window, cx);
    }

    fn home_of_active(&self) -> String {
        self.tab()
            .map(|t| t.home.clone())
            .unwrap_or_else(|| "/".into())
    }

    fn input_cancel(&mut self, _: &InputCancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_location {
            self.editing_location = false;
        } else if !self.filter_input.read(cx).text().is_empty() {
            self.filter_input.update(cx, |i, cx| i.clear(window, cx));
        }
        self.focus_list(window, cx);
    }

    fn input_down(&mut self, _: &InputDown, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut()
            && tab.browser.visible() > 0
        {
            tab.browser.focus(0, Move::Replace);
            tab.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        self.focus_list(window, cx);
    }

    fn toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.show_hidden = !self.settings.show_hidden;
        self.reorder_all();
        self.save_settings(cx);
        let msg = if self.settings.show_hidden {
            "Showing hidden files"
        } else {
            "Hiding hidden files"
        };
        self.show_toast(msg, ToastKind::Info, cx);
    }

    fn toggle_transfers(&mut self, _: &ToggleTransfers, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.show_transfers = !self.settings.show_transfers;
        self.save_settings(cx);
        cx.notify();
    }

    fn show_help(&mut self, _: &ShowHelp, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::Help, window, cx);
    }

    fn show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() || self.dialog.take().is_some() {
            self.focus_list(window, cx);
        } else if self.editing_location {
            self.editing_location = false;
            self.focus_list(window, cx);
        } else if !self.filter_input.read(cx).text().is_empty() {
            self.filter_input.update(cx, |i, cx| i.clear(window, cx));
            self.focus_list(window, cx);
        }
    }

    fn download_action(&mut self, _: &Download, _: &mut Window, cx: &mut Context<Self>) {
        self.download(None, cx);
    }

    fn download_to_action(&mut self, _: &DownloadTo, _: &mut Window, cx: &mut Context<Self>) {
        self.download_to(cx);
    }

    fn upload_files(&mut self, _: &UploadFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.pick_uploads(false, cx);
    }

    fn upload_folder(&mut self, _: &UploadFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.pick_uploads(true, cx);
    }

    fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_new_folder(window, cx);
    }

    fn rename(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_rename(window, cx);
    }

    fn delete_selected(&mut self, _: &DeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_delete(window, cx);
    }

    fn copy_url_action(&mut self, _: &CopyUrl, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_url(cx);
    }

    fn refresh_action(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active;
        if self.tab().is_some_and(|t| t.phase == Phase::Ready) {
            self.refresh_tab(ix);
        }
        cx.notify();
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    /// The window size is written once it stops changing, not every frame of
    /// a resize.
    /// Scrolls the list a few rows every frame, back to back, and reports the
    /// UI thread's CPU per frame. Starts once a listing has fully arrived.
    fn run_scroll_probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((left, last, samples)) = self.scroll_probe.as_mut() else {
            return;
        };
        let ready = self.tabs.get(self.active).is_some_and(|t| {
            t.phase == Phase::Ready && !t.browser.loading && t.browser.visible() > 0
        });
        if !ready {
            return;
        }
        let now = thread_cpu();
        if *last > 0.0 {
            samples.push(now - *last);
        }
        *last = now;
        if *left == 0 {
            let mut v = std::mem::take(samples);
            v.sort_by(f64::total_cmp);
            let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
            let p95 = v.get(v.len() * 95 / 100).copied().unwrap_or(0.0);
            println!(
                "scroll_frame_us_mean={:.0} scroll_frame_us_p95={:.0} frames={}",
                mean * 1e6,
                p95 * 1e6,
                v.len()
            );
            self.scroll_probe = None;
            cx.quit();
            return;
        }
        *left -= 1;
        let tab = &mut self.tabs[self.active];
        tab.browser.move_cursor(3, Move::Replace);
        tab.scroll
            .scroll_to_item(tab.browser.cursor, ScrollStrategy::Nearest);
        window.request_animation_frame();
    }

    fn remember_size(&mut self, window: &Window, cx: &mut Context<Self>) {
        let size = window.viewport_size();
        let now = (f32::from(size.width), f32::from(size.height));
        if (now.0 - self.window_size.0).abs() < 1.0 && (now.1 - self.window_size.1).abs() < 1.0 {
            return;
        }
        self.window_size = now;
        self.save_layout = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            this.update(cx, |this, cx| {
                this.settings.window.width = this.window_size.0;
                this.settings.window.height = this.window_size.1;
                this.save_settings(cx);
            })
            .ok();
        }));
    }
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("ferry/open")
}

fn describe_queued(files: usize, folders: usize, into: &std::path::Path) -> String {
    let what = match (files, folders) {
        (0, 1) => "1 folder".to_owned(),
        (0, n) => format!("{n} folders"),
        (f, 0) => format::plural(f as u64, "file", "files"),
        (f, n) => format!(
            "{} and {}",
            format::plural(f as u64, "file", "files"),
            format::plural(n as u64, "folder", "folders")
        ),
    };
    format!("Downloading {what} to {}", tilde(into))
}

fn describe_queued_upload(files: usize, folders: usize, into: &str) -> String {
    let what = match (files, folders) {
        (0, 1) => "1 folder".to_owned(),
        (0, n) => format!("{n} folders"),
        (f, 0) => format::plural(f as u64, "file", "files"),
        (f, n) => format!(
            "{} and {}",
            format::plural(f as u64, "file", "files"),
            format::plural(n as u64, "folder", "folders")
        ),
    };
    format!("Uploading {what} to {into}")
}

/// `/home/me/Downloads` → `~/Downloads`.
pub fn tilde(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.tab() {
            Some(tab) if tab.phase == Phase::Ready => {
                format!("{} — {}", tab.title(), tab.browser.path)
            }
            Some(tab) => tab.title(),
            None => "Ferry".to_owned(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        self.remember_size(window, cx);
        self.transfers.expire(TRANSFER_LINGER);
        self.next_question();

        // FERRY_STARTUP_PROBE=1: report time to the first frame and quit.
        if !self.probed_startup && std::env::var_os("FERRY_STARTUP_PROBE").is_some() {
            self.probed_startup = true;
            window.on_next_frame(|_, cx| {
                let ms = crate::STARTED
                    .get()
                    .map(|t| t.elapsed().as_secs_f64() * 1e3)
                    .unwrap_or(0.);
                println!("startup_ms={ms:.0}");
                cx.quit();
            });
        }

        if let Some(focus) = self.pending_focus.take() {
            window.focus(&focus, cx);
        }
        self.run_scroll_probe(window, cx);

        // Confirmations take the keyboard so Enter and Esc reach them.
        if (self.dialog.as_ref().is_some_and(Dialog::wants_dialog_focus) || self.asking_now())
            && !self.dialog_focus.is_focused(window)
        {
            window.focus(&self.dialog_focus, cx);
        }

        let signatures = self.panel_signatures(window, cx);
        self.panels.sync(signatures, cx);
        let drawer = self.transfers_height();

        div()
            .id("root")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::close_tab_action))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::edit_location))
            .on_action(cx.listener(Self::focus_filter))
            .on_action(cx.listener(Self::input_confirm))
            .on_action(cx.listener(Self::input_cancel))
            .on_action(cx.listener(Self::input_down))
            .on_action(cx.listener(Self::toggle_hidden))
            .on_action(cx.listener(Self::toggle_transfers))
            .on_action(cx.listener(Self::show_help))
            .on_action(cx.listener(Self::show_settings))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::download_action))
            .on_action(cx.listener(Self::download_to_action))
            .on_action(cx.listener(Self::upload_files))
            .on_action(cx.listener(Self::upload_folder))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename))
            .on_action(cx.listener(Self::delete_selected))
            .on_action(cx.listener(Self::copy_url_action))
            .on_action(cx.listener(Self::refresh_action))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.go_up(cx)))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.go_back(cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.go_forward(cx)),
            )
            .relative()
            .size_full()
            .flex()
            .bg(theme::bg())
            .text_color(theme::text())
            .text_size(px(13.))
            .child(
                self.panels
                    .sidebar
                    .clone()
                    .cached(panels::sized(Some(self.sidebar_width()), None)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .when(!self.tabs.is_empty(), |d| {
                        d.child(
                            self.panels
                                .tabs
                                .clone()
                                .cached(panels::sized(None, Some(38.))),
                        )
                        .child(
                            self.panels
                                .toolbar
                                .clone()
                                .cached(panels::sized(None, Some(50.))),
                        )
                        .child(self.render_body(window, cx))
                    })
                    .when(self.tabs.is_empty(), |d| d.child(self.render_welcome(cx)))
                    .when(drawer > 0., |d| {
                        d.child(
                            self.panels
                                .transfers
                                .clone()
                                .cached(panels::sized(None, Some(drawer))),
                        )
                    })
                    .when(!self.tabs.is_empty(), |d| {
                        d.child(
                            self.panels
                                .status
                                .clone()
                                .cached(panels::sized(None, Some(28.))),
                        )
                    }),
            )
            .children(self.render_toast(cx))
            .children(self.render_menu(window, cx))
            .children(self.render_dialog(window, cx))
    }
}

/// CPU time used by the calling thread, in seconds.
fn thread_cpu() -> f64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Hand freed heap pages back to the kernel.
pub fn release_free_memory() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0);
    }
}
