//! Made-up state for offscreen screenshots (`--features snapshot`).

use ferry::model::browser::Move;
use ferry::model::connection::{Auth, Protocol};
use ferry::model::entries::{Entries, Entry, Kind};
use ferry::model::message::{Event, SessionId};
use ferry::model::order::Filter;
use ferry::model::transfer::{Conflict, Direction, State};
use gpui::{Context, Point, ScrollStrategy, Window, px};

use super::{Dialog, Menu, Phase, Tab, Workspace};

const NOW: i64 = 1_793_000_000;

fn bookmarks(this: &mut Workspace) {
    let make =
        |this: &mut Workspace, protocol, name: &str, host: &str, user: &str, bucket: &str| {
            this.state.begin_new(protocol);
            let d = this.state.editor.as_mut().unwrap();
            d.connection.name = name.into();
            d.connection.host = host.into();
            d.connection.username = user.into();
            d.connection.bucket = bucket.into();
            if protocol == Protocol::Sftp {
                d.connection.auth = Auth::Password;
            }
            this.state.commit_edit();
        };
    make(
        this,
        Protocol::Sftp,
        "Production web",
        "web-01.example.com",
        "deploy",
        "",
    );
    make(
        this,
        Protocol::Sftp,
        "Build box",
        "build.internal",
        "ci",
        "",
    );
    make(this, Protocol::S3, "Media bucket", "", "", "acme-media");
    make(this, Protocol::Sftp, "Raspberry Pi", "pi.local", "pi", "");
    make(
        this,
        Protocol::S3,
        "Backups (MinIO)",
        "http://nas:9000",
        "",
        "backups",
    );
}

fn listing(big: usize) -> Entries {
    let mut e = Entries::new();
    let dirs = [
        ("assets", 0o755),
        ("config", 0o755),
        ("logs", 0o750),
        ("public_html", 0o755),
        ("releases", 0o755),
        (".ssh", 0o700),
    ];
    for (i, (name, mode)) in dirs.iter().enumerate() {
        e.push(Entry {
            name,
            size: 4096,
            modified: NOW - 3600 * (i as i64 * 7 + 2),
            mode: *mode,
            kind: Kind::Directory,
        });
    }
    let files: [(&str, u64, u32); 16] = [
        ("deploy.sh", 2_481, 0o755),
        ("nginx.conf", 3_920, 0o644),
        ("Quarterly Report.pdf", 1_482_113, 0o644),
        ("hero-banner@2x.png", 845_220, 0o644),
        ("database-2026-11-09.sql.gz", 248_331_002, 0o640),
        ("README.md", 7_310, 0o644),
        ("app.js", 182_004, 0o644),
        ("styles.css", 41_220, 0o644),
        ("intro.mp4", 61_000_441, 0o644),
        ("podcast-ep12.mp3", 38_002_118, 0o644),
        ("invoice_0042.xlsx", 22_011, 0o644),
        ("keynote.pptx", 9_401_222, 0o644),
        ("main.rs", 12_904, 0o644),
        ("archive.tar.zst", 734_002_111, 0o644),
        ("current", 0, 0o777),
        (".env", 512, 0o600),
    ];
    for (i, (name, size, mode)) in files.iter().enumerate() {
        e.push(Entry {
            name,
            size: *size,
            modified: NOW - 5400 * (i as i64 * 3 + 1) - 86400 * (i as i64 / 4),
            mode: *mode,
            kind: if *name == "current" {
                Kind::Symlink
            } else {
                Kind::File
            },
        });
    }
    let mut name = String::new();
    for i in 0..big {
        use std::fmt::Write as _;
        name.clear();
        let _ = write!(name, "file-{i:06}.dat");
        e.push(Entry {
            name: &name,
            size: (i as u64 * 7919) % 10_000_000,
            modified: NOW - i as i64 * 60,
            mode: 0o644,
            kind: Kind::File,
        });
    }
    e
}

fn ready_tab(this: &mut Workspace, conn_ix: usize, path: &str, big: usize, session: u32) -> usize {
    let connection = this.state.connections()[conn_ix].clone();
    let mut tab = Tab::new(this.next_tab, connection);
    this.next_tab += 1;
    tab.phase = Phase::Ready;
    tab.browser.session = Some(SessionId(session));
    tab.back.push("/home/deploy".into());
    let generation = tab.browser.navigate(path.to_owned());
    let mut event = Event::Listing {
        session: SessionId(session),
        generation,
        entries: listing(big),
        last: true,
    };
    tab.browser.apply(
        &mut event,
        this.settings.sort,
        Filter::new("", this.settings.show_hidden),
    );
    tab.browser.load_time = Some(std::time::Duration::from_micros(38_400));
    this.tabs.push(tab);
    this.tabs.len() - 1
}

fn transfers(this: &mut Workspace, conflict: bool) {
    let s = SessionId(1);
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let add = |this: &mut Workspace, dir, remote: &str, local: &str, total: u64| {
        let id = this
            .transfers
            .add(dir, remote.into(), home.join(local), total);
        this.transfer_session.insert(id, s);
        id
    };
    let a = add(
        this,
        Direction::Download,
        "/var/www/database-2026-11-09.sql.gz",
        "Downloads/database-2026-11-09.sql.gz",
        248_331_002,
    );
    this.transfers.start(a, 248_331_002);
    this.transfers.progress(a, 151_000_000);
    let b = add(
        this,
        Direction::Upload,
        "/var/www/public_html/hero-banner@2x.png",
        "Pictures/hero-banner@2x.png",
        845_220,
    );
    this.transfers.finish(b, State::Done);
    let c = add(
        this,
        Direction::Download,
        "/var/www/logs/error.log",
        "Downloads/error.log",
        0,
    );
    this.transfers.finish(
        c,
        State::Failed("Permission denied: /var/www/logs/error.log".into()),
    );
    if conflict {
        let d = add(
            this,
            Direction::Download,
            "/var/www/Quarterly Report.pdf",
            "Downloads/Quarterly Report.pdf",
            1_482_113,
        );
        this.transfers.blocked(
            d,
            Conflict {
                existing: Some(1_402_001),
                partial: Some(530_000),
                source: 1_482_113,
            },
        );
        let _ = add(
            this,
            Direction::Download,
            "/var/www/README.md",
            "Downloads/README.md",
            7_310,
        );
        this.asking = Some(d);
    }
}

impl Workspace {
    pub fn debug_fake(&mut self, scenario: &str, window: &mut Window, cx: &mut Context<Self>) {
        bookmarks(self);
        let select = |this: &mut Workspace, rows: &[usize]| {
            if let Some(tab) = this.tabs.get_mut(this.active) {
                for (i, &r) in rows.iter().enumerate() {
                    tab.browser
                        .focus(r, if i == 0 { Move::Replace } else { Move::Toggle });
                }
            }
        };
        match scenario {
            "welcome" => {}
            "list" | "menu" | "conflict" | "filter" => {
                ready_tab(self, 4, "/var/www", 0, 1);
                let mut connecting = Tab::new(self.next_tab, self.state.connections()[0].clone());
                self.next_tab += 1;
                connecting.phase = Phase::Connecting;
                self.tabs.push(connecting);
                self.active = 0;
                window.focus(&self.list_focus, cx);
                select(self, &[8, 10]);
                transfers(self, scenario == "conflict");
                if scenario == "menu" {
                    self.menu = Some(Menu::Row {
                        position: Point::new(px(560.), px(400.)),
                    });
                }
                if scenario == "filter" {
                    self.filter_input
                        .update(cx, |i, cx| i.set_text("re", None, cx));
                    self.set_filter("re".into(), cx);
                }
            }
            "big" => {
                ready_tab(self, 4, "/srv/big", 100_000, 1);
                self.active = 0;
                window.focus(&self.list_focus, cx);
                select(self, &[0]);
            }
            "empty" => {
                let ix = ready_tab(self, 1, "/tmp/empty", 0, 1);
                let tab = &mut self.tabs[ix];
                let generation = tab.browser.navigate("/home/ci/empty".to_owned());
                let mut event = Event::Listing {
                    session: SessionId(1),
                    generation,
                    entries: Entries::new(),
                    last: true,
                };
                tab.browser
                    .apply(&mut event, self.settings.sort, Filter::new("", false));
            }
            "failed" => {
                let mut tab = Tab::new(self.next_tab, self.state.connections()[0].clone());
                self.next_tab += 1;
                tab.phase = Phase::Failed(
                    "Authentication failed: the server rejected the password for deploy.".into(),
                );
                self.tabs.push(tab);
            }
            "editor" => {
                ready_tab(self, 4, "/var/www", 0, 1);
                self.edit_connection(None, Protocol::Sftp, window, cx);
            }
            "editor-s3" => {
                let id = self.state.connections()[2].id;
                self.edit_connection(Some(id), Protocol::S3, window, cx);
            }
            "secret" => {
                let mut tab = Tab::new(self.next_tab, self.state.connections()[0].clone());
                let id = tab.id;
                self.next_tab += 1;
                tab.phase = Phase::Unlocking { asked: false };
                self.tabs.push(tab);
                self.ask_secret(id, "Password", cx);
            }
            "settings" => {
                ready_tab(self, 4, "/var/www", 0, 1);
                self.open_dialog(Dialog::Settings, window, cx);
            }
            "help" => {
                ready_tab(self, 4, "/var/www", 0, 1);
                self.open_dialog(Dialog::Help, window, cx);
            }
            "delete" => {
                ready_tab(self, 4, "/var/www", 0, 1);
                select(self, &[2, 9, 10]);
                self.ask_delete(window, cx);
            }
            other => eprintln!("unknown scenario {other}"),
        }
        cx.notify();
    }

    /// One frame of the frame benchmark.
    pub fn debug_step(&mut self, step: bool, _: &mut Window, cx: &mut Context<Self>) {
        if step && let Some(tab) = self.tabs.get_mut(self.active) {
            tab.browser.move_cursor(7, Move::Replace);
            tab.scroll
                .scroll_to_item(tab.browser.cursor, ScrollStrategy::Nearest);
        }
        cx.notify();
    }
}

/// What the live end-to-end check needs to see.
pub struct LiveStatus {
    pub phase: String,
    pub loading: bool,
    pub path: String,
    pub names: Vec<String>,
    pub busy: bool,
    pub toast: String,
}

impl Workspace {
    /// Save a key-file SFTP connection and open it.
    #[allow(clippy::too_many_arguments)]
    pub fn debug_live_open(
        &mut self,
        host: &str,
        port: u16,
        user: &str,
        key: &str,
        start: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.begin_new(Protocol::Sftp);
        let d = self.state.editor.as_mut().unwrap();
        d.connection.name = "Live test".into();
        d.connection.host = host.into();
        d.connection.port = port;
        d.connection.username = user.into();
        d.connection.auth = Auth::KeyFile { path: key.into() };
        d.connection.initial_path = start.into();
        let id = d.connection.id;
        assert!(self.state.commit_edit(), "the test connection validates");
        self.open_connection(id, window, cx);
    }

    pub fn debug_live_status(&self) -> LiveStatus {
        let tab = self.tab();
        LiveStatus {
            phase: match tab.map(|t| &t.phase) {
                Some(Phase::Ready) => "ready".into(),
                Some(Phase::Connecting) => "connecting".into(),
                Some(Phase::Unlocking { .. }) => "unlocking".into(),
                Some(Phase::Failed(why)) => format!("failed: {why}"),
                None => "none".into(),
            },
            loading: tab.is_some_and(|t| t.browser.loading),
            path: tab.map(|t| t.browser.path.clone()).unwrap_or_default(),
            names: tab
                .map(|t| {
                    let e = t.browser.entries();
                    t.browser
                        .order()
                        .iter()
                        .map(|i| e.name(i).to_owned())
                        .collect()
                })
                .unwrap_or_default(),
            busy: self.transfers.active() > 0 || self.expanding > 0,
            toast: self
                .toast
                .as_ref()
                .map(|t| t.text.to_string())
                .unwrap_or_default(),
        }
    }

    /// One step of the live script.
    pub fn debug_live_do(
        &mut self,
        step: &str,
        arg: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let select = |this: &mut Workspace, name: &str| {
            if let Some(tab) = this.tabs.get_mut(this.active)
                && let Some(row) = tab.row_of(name)
            {
                tab.browser.focus(row, Move::Replace);
            }
        };
        match step {
            "mkdir" => self.create_folder(arg.into()),
            "rename" => {
                let (from, to) = arg.split_once('>').unwrap();
                let path = self.tab().unwrap().browser.path.clone();
                self.rename_to(ferry::model::path::join(&path, from), to.into());
            }
            "download" => {
                select(self, arg);
                self.download(None, cx);
            }
            "upload" => self.upload_paths(vec![arg.into()], None, cx),
            "delete" => {
                select(self, arg);
                let paths = self.chosen();
                self.delete_paths(paths);
            }
            "open" => {
                select(self, arg);
                let row = self.tab().unwrap().browser.cursor;
                self.open_row(row, cx);
            }
            "up" => self.go_up(cx),
            "filter" => self.set_filter(arg.into(), cx),
            _ => panic!("unknown step {step}"),
        }
        let _ = window;
        cx.notify();
    }
}
