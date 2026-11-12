//! Modal dialogs: the connection editor, the password prompt, naming things,
//! confirming deletes, transfer conflicts, settings and the keyboard help.

use ferry::model::connection::{Auth, Connection, ConnectionId, Protocol};
use ferry::model::order::SortKey;
use ferry::model::path;
use ferry::model::settings::{Clock, Theme};
use ferry::model::transfer::{Direction, Resolution, State};
use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, Hsla, PathPromptOptions, SharedString,
    Subscription, Window, deferred, div, prelude::*, px,
};

use super::{
    Workspace, DialogCancel, DialogConfirm, DialogNext, DialogPrev, Phase, ToastKind, tilde,
};
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme;
use crate::ui::widgets::{button, icon_el, segment, segmented, switch};
use crate::ui::format;

pub enum NameKind {
    NewFolder,
    Rename { from: String },
}

/// The connection editor's fields. The draft itself lives in
/// `Bookmarks::editor`, where the keychain's "is a secret saved?" answers
/// land.
pub struct Editor {
    name: Entity<TextInput>,
    host: Entity<TextInput>,
    port: Entity<TextInput>,
    username: Entity<TextInput>,
    key_path: Entity<TextInput>,
    profile: Entity<TextInput>,
    access_key: Entity<TextInput>,
    bucket: Entity<TextInput>,
    region: Entity<TextInput>,
    initial_path: Entity<TextInput>,
    secret: Entity<TextInput>,
    problems: Vec<SharedString>,
    _subs: Vec<Subscription>,
}

pub enum Dialog {
    Editor(Box<Editor>),
    Secret {
        tab: u64,
        prompt: &'static str,
        input: Entity<TextInput>,
        remember: bool,
    },
    Name {
        kind: NameKind,
        input: Entity<TextInput>,
        error: Option<SharedString>,
        _sub: Subscription,
    },
    Delete {
        paths: Vec<(String, bool)>,
    },
    RemoveBookmark {
        id: ConnectionId,
        name: String,
    },
    Settings,
    Help,
}

impl Dialog {
    /// Dialogs without a text field take the keyboard themselves, so Enter
    /// and Esc reach them.
    pub fn wants_dialog_focus(&self) -> bool {
        matches!(
            self,
            Dialog::Delete { .. } | Dialog::RemoveBookmark { .. } | Dialog::Settings | Dialog::Help
        )
    }
}

fn input(cx: &mut Context<Workspace>, placeholder: &str, text: &str) -> Entity<TextInput> {
    let text = text.to_owned();
    let placeholder = placeholder.to_owned();
    cx.new(|cx| {
        let mut i = TextInput::new(placeholder, cx);
        i.set_text(text, None, cx);
        i
    })
}

impl Workspace {
    pub(super) fn open_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.dialog = Some(dialog);
        if self.dialog.as_ref().is_some_and(Dialog::wants_dialog_focus) {
            window.focus(&self.dialog_focus, cx);
        }
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.dialog, Some(Dialog::Editor(_))) {
            self.state.cancel_edit();
        }
        if let Some(Dialog::Secret { tab, .. }) = &self.dialog
            && let Some(ix) = self.tab_ix_by_id(*tab)
            && matches!(self.tabs[ix].phase, Phase::Unlocking { .. })
        {
            self.tabs[ix].phase = Phase::Failed("No password was given.".into());
        }
        self.dialog = None;
        self.focus_list(window, cx);
    }

    /// Whether the conflict question is the thing on screen.
    pub(super) fn asking_now(&self) -> bool {
        self.dialog.is_none() && self.asking.is_some()
    }

    /// Pick the next blocked transfer to ask about.
    pub(super) fn next_question(&mut self) {
        if let Some(id) = self.asking
            && self
                .transfers
                .get(id)
                .is_some_and(|t| matches!(t.state, State::Blocked(_)))
        {
            return;
        }
        self.asking = self.transfers.blocked_ids().first().copied();
    }

    // ── Opening dialogs ─────────────────────────────────────────────────

    pub(crate) fn edit_connection(
        &mut self,
        id: Option<ConnectionId>,
        protocol: Protocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            Some(id) => self.state.begin_edit(id),
            None => self.state.begin_new(protocol),
        }
        self.start_editor(window, cx);
    }

    pub(crate) fn duplicate_connection(
        &mut self,
        id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.duplicate(id);
        self.start_editor(window, cx);
    }

    fn start_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.state.editor.as_ref() else {
            return;
        };
        let c = draft.connection.clone();
        if draft.existing {
            self.creds.check_saved(&c);
        }
        self.creds.refresh_agent();
        let (key_path, profile, access_key) = match &c.auth {
            Auth::KeyFile { path } => (path.clone(), "default".to_owned(), String::new()),
            Auth::AwsProfile { profile } => (String::new(), profile.clone(), String::new()),
            Auth::AwsKeys { access_key_id } => {
                (String::new(), "default".to_owned(), access_key_id.clone())
            }
            _ => (String::new(), "default".to_owned(), String::new()),
        };
        let editor = Editor {
            name: input(cx, "My server", &c.name),
            host: input(
                cx,
                if c.protocol == Protocol::S3 {
                    "Leave empty for AWS"
                } else {
                    "example.com"
                },
                &c.host,
            ),
            port: input(cx, "22", &c.port.to_string()),
            username: input(cx, "username", &c.username),
            key_path: input(cx, "~/.ssh/id_ed25519", &key_path),
            profile: input(cx, "default", &profile),
            access_key: input(cx, "AKIA…", &access_key),
            bucket: input(cx, "my-bucket", &c.bucket),
            region: input(cx, "us-east-1", &c.region),
            initial_path: input(cx, "Home directory", &c.initial_path),
            secret: cx.new(|cx| TextInput::new("", cx).masked()),
            problems: Vec::new(),
            _subs: Vec::new(),
        };
        let first = editor.name.focus_handle(cx);
        self.open_dialog(Dialog::Editor(Box::new(editor)), window, cx);
        window.focus(&first, cx);
    }

    pub(crate) fn ask_secret(&mut self, tab: u64, prompt: &'static str, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(prompt, cx).masked());
        self.menu = None;
        self.dialog = Some(Dialog::Secret {
            tab,
            prompt,
            input: input.clone(),
            remember: true,
        });
        // Focus is taken on the next frame, once the field is on screen.
        self.pending_focus = Some(input.focus_handle(cx));
        cx.notify();
    }

    pub(crate) fn ask_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let mut name = "New Folder".to_owned();
        let mut n = 2;
        while tab.row_of(&name).is_some() {
            name = format!("New Folder {n}");
            n += 1;
        }
        let len = name.len();
        self.name_dialog(NameKind::NewFolder, name, 0..len, window, cx);
    }

    pub(crate) fn ask_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.chosen();
        let [(from, is_dir)] = chosen.as_slice() else {
            if chosen.len() > 1 {
                self.show_toast("Select one item to rename", ToastKind::Info, cx);
            }
            return;
        };
        let name = path::file_name(from).to_owned();
        // Select the name without its extension, like every file manager.
        let stem = if *is_dir {
            name.len()
        } else {
            match name.rfind('.') {
                Some(0) | None => name.len(),
                Some(i) => i,
            }
        };
        self.name_dialog(
            NameKind::Rename { from: from.clone() },
            name,
            0..stem,
            window,
            cx,
        );
    }

    fn name_dialog(
        &mut self,
        kind: NameKind,
        text: String,
        select: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Name", cx);
            i.set_text(text, Some(select), cx);
            i
        });
        let sub = cx.subscribe_in(&input, window, |this, _, _: &TextInputEvent, _, cx| {
            this.validate_name(cx);
            cx.notify();
        });
        let focus = input.focus_handle(cx);
        self.open_dialog(
            Dialog::Name {
                kind,
                input,
                error: None,
                _sub: sub,
            },
            window,
            cx,
        );
        window.focus(&focus, cx);
    }

    fn validate_name(&mut self, cx: &mut Context<Self>) {
        let problem = match &self.dialog {
            Some(Dialog::Name { kind, input, .. }) => {
                let name = input.read(cx).text().trim().to_owned();
                let original = match kind {
                    NameKind::Rename { from } => Some(path::file_name(from).to_owned()),
                    NameKind::NewFolder => None,
                };
                name_problem(&name, original.as_deref(), |n| {
                    self.tab().is_some_and(|t| t.row_of(n).is_some())
                })
            }
            _ => return,
        };
        if let Some(Dialog::Name { error, .. }) = &mut self.dialog {
            *error = problem.map(Into::into);
        }
    }

    pub(crate) fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.chosen();
        if paths.is_empty() {
            return;
        }
        if !self.settings.confirm_delete {
            self.delete_paths(paths);
            return;
        }
        self.open_dialog(Dialog::Delete { paths }, window, cx);
    }

    pub(crate) fn ask_remove_bookmark(
        &mut self,
        id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = self.state.get(id) else {
            return;
        };
        let name = c.name.clone();
        self.open_dialog(Dialog::RemoveBookmark { id, name }, window, cx);
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::Settings, window, cx);
    }

    // ── Keys ────────────────────────────────────────────────────────────

    fn dialog_confirm(&mut self, _: &DialogConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.asking_now() {
            // Enter means the safe choice: keep what is there.
            self.resolve_conflict(Resolution::Skip, cx);
            return;
        }
        match self.dialog.take() {
            Some(Dialog::Editor(editor)) => {
                self.dialog = Some(Dialog::Editor(editor));
                self.save_editor(false, window, cx);
            }
            Some(Dialog::Secret {
                tab,
                input,
                remember,
                ..
            }) => {
                let typed = input.read(cx).text().to_owned();
                self.connect_with_secret(tab, typed, remember);
                self.focus_list(window, cx);
            }
            Some(Dialog::Name {
                kind,
                input,
                error,
                _sub,
            }) => {
                let name = input.read(cx).text().trim().to_owned();
                if error.is_some() || name.is_empty() {
                    self.dialog = Some(Dialog::Name {
                        kind,
                        input,
                        error,
                        _sub,
                    });
                    return;
                }
                match kind {
                    NameKind::NewFolder => self.create_folder(name),
                    NameKind::Rename { from } => {
                        if path::file_name(&from) != name {
                            self.rename_to(from, name)
                        }
                    }
                }
                self.focus_list(window, cx);
            }
            Some(Dialog::Delete { paths }) => {
                self.delete_paths(paths);
                self.focus_list(window, cx);
            }
            Some(Dialog::RemoveBookmark { id, name }) => {
                if let Some(c) = self.state.get(id) {
                    self.creds.forget(c);
                }
                self.state.remove(id);
                self.show_toast(format!("Deleted “{name}”"), ToastKind::Info, cx);
                self.focus_list(window, cx);
            }
            Some(Dialog::Settings) | Some(Dialog::Help) | None => self.focus_list(window, cx),
        }
        cx.notify();
    }

    fn dialog_cancel(&mut self, _: &DialogCancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.asking_now() {
            self.resolve_conflict(Resolution::Skip, cx);
            return;
        }
        self.close_dialog(window, cx);
    }

    fn dialog_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Editor(editor)) = &self.dialog else {
            return;
        };
        let Some(draft) = &self.state.editor else {
            return;
        };
        let fields = editor.fields(&draft.connection);
        let at = fields
            .iter()
            .position(|f| f.focus_handle(cx).is_focused(window))
            .unwrap_or(fields.len() - 1);
        let next = if forward {
            (at + 1) % fields.len()
        } else {
            (at + fields.len() - 1) % fields.len()
        };
        let focus = fields[next].focus_handle(cx);
        window.focus(&focus, cx);
    }

    // ── The editor ──────────────────────────────────────────────────────

    /// Copy the fields into the draft. Returns the typed secret.
    fn write_back(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let Some(Dialog::Editor(editor)) = &mut self.dialog else {
            return None;
        };
        let draft = self.state.editor.as_mut()?;
        let text = |e: &Entity<TextInput>| e.read(cx).text().trim().to_owned();
        let c = &mut draft.connection;
        c.name = text(&editor.name);
        c.host = text(&editor.host);
        c.username = text(&editor.username);
        c.bucket = text(&editor.bucket);
        c.region = text(&editor.region);
        c.initial_path = text(&editor.initial_path);
        c.auth = match &c.auth {
            Auth::Agent => Auth::Agent,
            Auth::Password => Auth::Password,
            Auth::KeyFile { .. } => Auth::KeyFile {
                path: text(&editor.key_path),
            },
            Auth::AwsProfile { .. } => Auth::AwsProfile {
                profile: text(&editor.profile),
            },
            Auth::AwsKeys { .. } => Auth::AwsKeys {
                access_key_id: text(&editor.access_key),
            },
        };
        let port = text(&editor.port);
        editor.problems.clear();
        if c.protocol == Protocol::Sftp {
            match port.parse::<u16>() {
                Ok(p) if p > 0 => c.port = p,
                _ => editor.problems.push("Port must be a number from 1 to 65535".into()),
            }
        }
        editor
            .problems
            .extend(c.problems().into_iter().map(SharedString::from));
        Some(editor.secret.read(cx).text().to_owned())
    }

    fn save_editor(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(typed) = self.write_back(cx) else {
            return;
        };
        let ok = matches!(&self.dialog, Some(Dialog::Editor(e)) if e.problems.is_empty());
        if !ok {
            cx.notify();
            return;
        }
        let Some(draft) = self.state.editor.as_ref() else {
            return;
        };
        let connection = draft.connection.clone();
        if !self.state.commit_edit() {
            cx.notify();
            return;
        }
        self.creds.save(&connection, &typed);
        // Open tabs keep their session but show the new name.
        for tab in self.tabs.iter_mut().filter(|t| t.connection.id == connection.id) {
            tab.connection = connection.clone();
        }
        self.dialog = None;
        self.show_toast(format!("Saved “{}”", connection.name), ToastKind::Success, cx);
        if connect {
            self.open_connection(connection.id, window, cx);
        } else {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    fn set_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        self.write_back(cx);
        let Some(draft) = self.state.editor.as_mut() else {
            return;
        };
        if draft.connection.protocol == protocol {
            return;
        }
        let mut fresh = Connection::new(draft.connection.id, protocol);
        fresh.name = draft.connection.name.clone();
        draft.connection = fresh;
        if let Some(Dialog::Editor(editor)) = &self.dialog {
            let port = protocol.default_port().to_string();
            let region = draft.connection.region.clone();
            editor.port.update(cx, |i, cx| i.set_text(port, None, cx));
            editor.region.update(cx, |i, cx| i.set_text(region, None, cx));
            editor.host.update(cx, |i, cx| i.set_text("", None, cx));
        }
        cx.notify();
    }

    fn set_auth(&mut self, make: fn() -> Auth, cx: &mut Context<Self>) {
        self.write_back(cx);
        if let Some(draft) = self.state.editor.as_mut() {
            let auth = make();
            if std::mem::discriminant(&auth) != std::mem::discriminant(&draft.connection.auth) {
                draft.connection.auth = auth;
                draft.secret_stored = false;
                self.creds.check_saved(&draft.connection);
            }
        }
        // Write the fields back again so the new variant picks up its field.
        self.write_back(cx);
        if let Some(Dialog::Editor(e)) = &mut self.dialog {
            e.problems.clear();
        }
        cx.notify();
    }

    // ── Rendering ───────────────────────────────────────────────────────

    pub(super) fn render_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let body = match self.dialog.as_ref() {
            Some(Dialog::Editor(_)) => self.render_editor(window, cx),
            Some(Dialog::Secret {
                tab,
                prompt,
                input,
                remember,
            }) => {
                let (title, summary) = self
                    .tab_ix_by_id(*tab)
                    .map(|ix| (self.tabs[ix].title(), self.tabs[ix].connection.summary()))
                    .unwrap_or_default();
                let remember = *remember;
                let focused = input.focus_handle(cx).is_focused(window);
                self.dialog_card(
                    "key",
                    theme::accent(),
                    format!("Connect to {title}").into(),
                    Some(summary.into()),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(field_box(prompt, input.clone(), focused, false))
                        .child(
                            div()
                                .id("remember")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .text_sm()
                                .text_color(theme::text_soft())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(Dialog::Secret { remember, .. }) = &mut this.dialog {
                                        *remember = !*remember;
                                    }
                                    cx.notify();
                                }))
                                .child(switch("remember-switch", remember))
                                .child(if self.creds.keychain_problem().is_some() {
                                    "Remember (no keychain available)"
                                } else {
                                    "Remember in the keychain"
                                }),
                        )
                        .into_any_element(),
                    vec![
                        button("d-cancel", "Cancel", None, false)
                            .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                            .into_any_element(),
                        button("d-ok", "Connect", Some("plug"), true)
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.dialog_confirm(&DialogConfirm, w, cx)
                            }))
                            .into_any_element(),
                    ],
                )
            }
            Some(Dialog::Name {
                kind, input, error, ..
            }) => {
                let (title, action, icon_name) = match kind {
                    NameKind::Rename { from } => (
                        format!("Rename “{}”", path::file_name(from)),
                        "Rename",
                        "pencil",
                    ),
                    NameKind::NewFolder => ("New folder".to_string(), "Create", "folder-plus"),
                };
                let error = error.clone();
                let blocked = error.is_some();
                let where_ = self
                    .tab()
                    .map(|t| t.browser.path.clone())
                    .unwrap_or_default();
                let focused = input.focus_handle(cx).is_focused(window);
                self.dialog_card(
                    icon_name,
                    theme::accent(),
                    title.into(),
                    Some(format!("In {where_}").into()),
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(field_box("Name", input.clone(), focused, blocked))
                        .children(
                            error.map(|e| div().text_xs().text_color(theme::danger()).child(e)),
                        )
                        .into_any_element(),
                    vec![
                        button("d-cancel", "Cancel", None, false)
                            .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                            .into_any_element(),
                        button("d-ok", action, None, true)
                            .when(blocked, |b| b.opacity(0.5))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.dialog_confirm(&DialogConfirm, w, cx)
                            }))
                            .into_any_element(),
                    ],
                )
            }
            Some(Dialog::Delete { paths }) => {
                let title = if paths.len() == 1 {
                    format!("Delete “{}”?", path::file_name(&paths[0].0))
                } else {
                    format!("Delete {}?", format::items(paths.len() as u64))
                };
                let dirs = paths.iter().filter(|(_, d)| *d).count();
                let subtitle = if dirs > 0 {
                    "Folders are deleted with everything inside them. This can't be undone."
                } else {
                    "They are removed from the server. This can't be undone."
                };
                self.dialog_card(
                    "alert",
                    theme::danger(),
                    title.into(),
                    Some(subtitle.into()),
                    name_list(paths.iter().map(|(p, d)| (path::file_name(p).to_owned(), *d)).collect()),
                    vec![
                        button("d-cancel", "Cancel", None, false)
                            .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                            .into_any_element(),
                        danger_button("d-ok", "Delete")
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.dialog_confirm(&DialogConfirm, w, cx)
                            }))
                            .into_any_element(),
                    ],
                )
            }
            Some(Dialog::RemoveBookmark { name, .. }) => self.dialog_card(
                "trash",
                theme::danger(),
                format!("Delete the bookmark “{name}”?").into(),
                Some("Its saved password is removed from the keychain too. Nothing on the server is touched.".into()),
                div().into_any_element(),
                vec![
                    button("d-cancel", "Cancel", None, false)
                        .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                        .into_any_element(),
                    danger_button("d-ok", "Delete bookmark")
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.dialog_confirm(&DialogConfirm, w, cx)
                        }))
                        .into_any_element(),
                ],
            ),
            Some(Dialog::Settings) => self.render_settings(cx),
            Some(Dialog::Help) => self.render_help(cx),
            None if self.asking.is_some() => self.render_conflict(cx)?,
            None => return None,
        };
        Some(
            deferred(
                div()
                    .id("dialog-backdrop")
                    .key_context("Dialog")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::overlay())
                    .on_action(cx.listener(Self::dialog_confirm))
                    .on_action(cx.listener(Self::dialog_cancel))
                    .on_action(cx.listener(|this, _: &DialogNext, w, cx| this.dialog_step(true, w, cx)))
                    .on_action(cx.listener(|this, _: &DialogPrev, w, cx| this.dialog_step(false, w, cx)))
                    .on_click(cx.listener(|this, _, w, cx| {
                        // A click outside closes only the dialogs that hold
                        // nothing typed.
                        if this
                            .dialog
                            .as_ref()
                            .is_some_and(|d| matches!(d, Dialog::Help | Dialog::Settings))
                        {
                            this.close_dialog(w, cx)
                        }
                    }))
                    .child(body),
            )
            .with_priority(3),
        )
    }

    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (Some(Dialog::Editor(editor)), Some(draft)) = (&self.dialog, &self.state.editor) else {
            return div().into_any_element();
        };
        let c = &draft.connection;
        let protocol = c.protocol;
        let existing = draft.existing;
        let focused = |e: &Entity<TextInput>| e.focus_handle(cx).is_focused(window);

        let mut form = div().flex().flex_col().gap_3();

        // Protocol: only for a new connection; changing an existing one's
        // protocol would orphan its keychain entry under the wrong slot.
        if !existing {
            form = form.child(
                segmented()
                    .child(
                        segment("p-sftp", "SFTP", Some("server"), protocol == Protocol::Sftp)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_protocol(Protocol::Sftp, cx)
                            })),
                    )
                    .child(
                        segment("p-s3", "Amazon S3", Some("cloud"), protocol == Protocol::S3)
                            .on_click(cx.listener(|this, _, _, cx| this.set_protocol(Protocol::S3, cx))),
                    ),
            );
        }

        form = form.child(labeled("Name", field_box("", editor.name.clone(), focused(&editor.name), false)));

        match protocol {
            Protocol::Sftp => {
                form = form.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div().flex_1().child(labeled(
                                "Server",
                                field_box("", editor.host.clone(), focused(&editor.host), false),
                            )),
                        )
                        .child(div().w(px(90.)).child(labeled(
                            "Port",
                            field_box("", editor.port.clone(), focused(&editor.port), false),
                        ))),
                );
                form = form.child(labeled(
                    "Username",
                    field_box("", editor.username.clone(), focused(&editor.username), false),
                ));
            }
            Protocol::S3 => {
                form = form.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().child(labeled(
                            "Bucket",
                            field_box("", editor.bucket.clone(), focused(&editor.bucket), false),
                        )))
                        .child(div().w(px(140.)).child(labeled(
                            "Region",
                            field_box("", editor.region.clone(), focused(&editor.region), false),
                        ))),
                );
                form = form.child(labeled(
                    "Endpoint (for MinIO, R2, Wasabi…)",
                    field_box("", editor.host.clone(), focused(&editor.host), false),
                ));
            }
        }

        // Authentication.
        let options = Auth::options_for(protocol);
        let mut seg = segmented();
        for (i, make) in options.iter().enumerate() {
            let a = make();
            let on = std::mem::discriminant(&a) == std::mem::discriminant(&c.auth);
            let make = *make;
            let icon_name = match a {
                Auth::Agent => "shield",
                Auth::Password => "lock",
                Auth::KeyFile { .. } => "key",
                Auth::AwsProfile { .. } => "file-text",
                Auth::AwsKeys { .. } => "key",
            };
            seg = seg.child(
                segment(("auth", i), a.label(), Some(icon_name), on)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_auth(make, cx))),
            );
        }
        form = form.child(labeled_el("Sign in with", seg.into_any_element()));

        let secret_label = match &c.auth {
            Auth::Password => Some("Password"),
            Auth::KeyFile { .. } => Some("Key passphrase (if the key has one)"),
            Auth::AwsKeys { .. } => Some("Secret access key"),
            _ => None,
        };
        match &c.auth {
            Auth::Agent => {
                let (text, color): (String, Hsla) = match self.creds.agent_keys() {
                    Ok([]) => ("The SSH agent is running but holds no keys. Add one with ssh-add.".into(), theme::warning()),
                    Ok(keys) => (
                        format!(
                            "{} in the agent: {}",
                            format::plural(keys.len() as u64, "key", "keys"),
                            keys.iter().map(|k| k.describe()).collect::<Vec<_>>().join(", ")
                        ),
                        theme::muted(),
                    ),
                    Err(e) => (e.to_string(), theme::warning()),
                };
                form = form.child(
                    div()
                        .flex()
                        .gap_2()
                        .items_start()
                        .text_xs()
                        .text_color(color)
                        .child(icon_el("shield", color, 13.))
                        .child(div().flex_1().child(text)),
                );
            }
            Auth::KeyFile { .. } => {
                form = form.child(labeled(
                    "Private key file",
                    field_box("", editor.key_path.clone(), focused(&editor.key_path), false),
                ));
            }
            Auth::AwsProfile { .. } => {
                form = form.child(labeled(
                    "Profile from ~/.aws",
                    field_box("", editor.profile.clone(), focused(&editor.profile), false),
                ));
            }
            Auth::AwsKeys { .. } => {
                form = form.child(labeled(
                    "Access key ID",
                    field_box("", editor.access_key.clone(), focused(&editor.access_key), false),
                ));
            }
            Auth::Password => {}
        }
        if let Some(label) = secret_label {
            let hint = if draft.secret_stored {
                "Saved in the keychain — leave empty to keep it"
            } else if self.creds.keychain_problem().is_some() {
                "No keychain available — you'll be asked each time"
            } else {
                "Stored in the system keychain, never in the bookmarks file"
            };
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(labeled(
                        label,
                        field_box("", editor.secret.clone(), focused(&editor.secret), false),
                    ))
                    .child(div().text_xs().text_color(theme::faint()).child(hint)),
            );
        }
        form = form.child(labeled(
            "Start in (optional)",
            field_box("", editor.initial_path.clone(), focused(&editor.initial_path), false),
        ));

        if !editor.problems.is_empty() {
            form = form.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme::danger().opacity(0.1))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .text_xs()
                    .text_color(theme::danger())
                    .children(editor.problems.iter().cloned()),
            );
        }

        let title: SharedString = if existing {
            "Edit connection".into()
        } else {
            "New connection".into()
        };
        self.dialog_card(
            if protocol == Protocol::S3 { "cloud" } else { "server" },
            theme::accent(),
            title,
            Some(
                if existing {
                    "Changes apply the next time you connect."
                } else {
                    "Bookmarks are kept in ~/.config/ferry/connections.toml."
                }
                .into(),
            ),
            form.into_any_element(),
            vec![
                button("d-cancel", "Cancel", None, false)
                    .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                    .into_any_element(),
                button("d-save", "Save", None, false)
                    .on_click(cx.listener(|this, _, w, cx| this.save_editor(false, w, cx)))
                    .into_any_element(),
                button("d-connect", "Save & connect", Some("plug"), true)
                    .on_click(cx.listener(|this, _, w, cx| this.save_editor(true, w, cx)))
                    .into_any_element(),
            ],
        )
    }

    fn render_conflict(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.asking?;
        let t = self.transfers.get(id)?.clone();
        let State::Blocked(conflict) = t.state else {
            return None;
        };
        let others = self.transfers.blocked_ids().len().saturating_sub(1)
            + self
                .transfers
                .all()
                .iter()
                .filter(|x| x.state == State::Queued)
                .count();
        let where_ = match t.direction {
            Direction::Download => "on this computer",
            Direction::Upload => "on the server",
        };
        let mut facts = div().flex().flex_col().gap_1().text_sm();
        let fact = |label: &str, value: String| {
            div()
                .flex()
                .gap_3()
                .child(div().w(px(150.)).text_color(theme::faint()).child(label.to_owned()))
                .child(div().text_color(theme::text_soft()).child(value))
        };
        facts = facts.child(fact("Coming in", format::bytes(conflict.source)));
        if let Some(existing) = conflict.existing {
            facts = facts.child(fact("Already there", format::bytes(existing)));
        }
        if let Some(partial) = conflict.partial {
            facts = facts.child(fact(
                "Interrupted earlier",
                format!(
                    "{} of {}",
                    format::bytes(partial),
                    format::bytes(conflict.source)
                ),
            ));
        }
        let can_resume = conflict.can_resume();
        let all = self.conflict_all;
        let content = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(facts)
            .when(can_resume, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme::faint())
                        .child("Continuing assumes the file hasn't changed since it was interrupted."),
                )
            })
            .when(others > 0, |d| {
                d.child(
                    div()
                        .id("conflict-all")
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(theme::text_soft())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.conflict_all = !this.conflict_all;
                            cx.notify();
                        }))
                        .child(switch("conflict-all-switch", all))
                        .child(format!("Do the same for the rest of this batch ({others} more)")),
                )
            })
            .into_any_element();
        let mut buttons = vec![
            button("c-skip", "Skip", None, false)
                .on_click(cx.listener(|this, _, _, cx| this.resolve_conflict(Resolution::Skip, cx)))
                .into_any_element(),
            button("c-both", "Keep both", None, false)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.resolve_conflict(Resolution::KeepBoth, cx)
                }))
                .into_any_element(),
        ];
        if can_resume {
            buttons.push(
                button("c-resume", "Continue", Some("arrow-right"), false)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.resolve_conflict(Resolution::Resume, cx)
                    }))
                    .into_any_element(),
            );
        }
        buttons.push(
            button("c-replace", "Replace", None, true)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.resolve_conflict(Resolution::Overwrite, cx)
                }))
                .into_any_element(),
        );
        Some(self.dialog_card(
            "copy",
            theme::warning(),
            format!("“{}” already exists", t.name).into(),
            Some(format!("There is already a file with this name {where_}.").into()),
            content,
            buttons,
        ))
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.settings.clone();
        let row = |title: &'static str, detail: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_4()
                .py_2()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().text_color(theme::text()).child(title))
                        .child(div().text_xs().text_color(theme::faint()).child(detail)),
                )
                .child(control)
        };
        let toggle = |id: &'static str, on: bool, f: fn(&mut Workspace)| {
            switch(id, on)
                .on_click(cx.listener(move |this, _, _, cx| {
                    f(this);
                    this.reorder_all();
                    this.save_settings(cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        let theme_seg = {
            let mut seg = segmented().w(px(240.));
            for (i, t) in [Theme::System, Theme::Light, Theme::Dark].into_iter().enumerate() {
                let label = match t {
                    Theme::System => "System",
                    Theme::Light => "Light",
                    Theme::Dark => "Dark",
                };
                seg = seg.child(segment(("theme", i), label, None, s.theme == t).on_click(
                    cx.listener(move |this, _, w, cx| {
                        this.settings.theme = t;
                        this.apply_theme(w);
                        this.save_settings(cx);
                        cx.notify();
                    }),
                ));
            }
            seg.into_any_element()
        };
        let clock_seg = {
            let mut seg = segmented().w(px(160.));
            for (i, c) in [Clock::Local, Clock::Utc].into_iter().enumerate() {
                let label = match c {
                    Clock::Local => "Local",
                    Clock::Utc => "UTC",
                };
                seg = seg.child(segment(("clock", i), label, None, s.clock == c).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.settings.clock = c;
                        this.save_settings(cx);
                        cx.notify();
                    }),
                ));
            }
            seg.into_any_element()
        };
        let downloads = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .max_w(px(200.))
                    .truncate()
                    .text_sm()
                    .text_color(theme::muted())
                    .child(tilde(&s.downloads())),
            )
            .child(
                button("pick-downloads", "Change…", None, false).on_click(cx.listener(
                    |_, _, _, cx| {
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
                                this.update(cx, |this, cx| {
                                    this.settings.download_dir = dir.display().to_string();
                                    this.save_settings(cx);
                                    cx.notify();
                                })
                                .ok();
                            }
                        })
                        .detach();
                    },
                )),
            )
            .into_any_element();
        let content = div()
            .flex()
            .flex_col()
            .child(row("Appearance", "Follow the system, or pick one.", theme_seg))
            .child(row("Downloads", "Where Download puts things. Download to… asks.", downloads))
            .child(row(
                "Show hidden files",
                "Names that start with a dot.",
                toggle("s-hidden", s.show_hidden, |t| {
                    t.settings.show_hidden = !t.settings.show_hidden
                }),
            ))
            .child(row(
                "Folders first",
                "Keep folders above files whatever the sort.",
                toggle("s-dirs", s.sort.directories_first, |t| {
                    t.settings.sort.directories_first = !t.settings.sort.directories_first
                }),
            ))
            .child(row(
                "Ask before deleting",
                "Deleting on a server can't be undone.",
                toggle("s-confirm", s.confirm_delete, |t| {
                    t.settings.confirm_delete = !t.settings.confirm_delete
                }),
            ))
            .child(row("Times", "Show modification times in…", clock_seg))
            .into_any_element();
        let _ = SortKey::Name;
        self.dialog_card(
            "settings",
            theme::accent(),
            "Settings".into(),
            Some("Saved as you change them.".into()),
            content,
            vec![
                button("d-done", "Done", None, true)
                    .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                    .into_any_element(),
            ],
        )
    }

    fn render_help(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const GROUPS: &[(&str, &[(&str, &str)])] = &[
            (
                "Moving around",
                &[
                    ("↑ ↓  PgUp PgDn  Home End", "Move"),
                    ("Shift + move, Ctrl/Shift + click", "Select more"),
                    ("Enter, double-click", "Open folder or file"),
                    ("Backspace, Alt+↑", "Enclosing folder"),
                    ("Alt+← / Alt+→", "Back / forward"),
                    ("Ctrl+L, F6", "Type a path"),
                    ("Ctrl+F", "Filter"),
                    ("Typing", "Jump to a name"),
                ],
            ),
            (
                "Files",
                &[
                    ("Ctrl+D, F3", "Download"),
                    ("Ctrl+Shift+D", "Download to…"),
                    ("Ctrl+U / Ctrl+Shift+U", "Upload files / folder"),
                    ("Drop on the window", "Upload"),
                    ("Drag onto a folder", "Move into it"),
                    ("F2", "Rename"),
                    ("Delete", "Delete"),
                    ("Ctrl+Shift+N", "New folder"),
                    ("Ctrl+Shift+C", "Copy URL"),
                ],
            ),
            (
                "Window",
                &[
                    ("Ctrl+N", "New connection"),
                    ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
                    ("Ctrl+W", "Disconnect"),
                    ("F5, Ctrl+R", "Refresh"),
                    ("Ctrl+H", "Hidden files"),
                    ("Ctrl+J", "Transfers"),
                    ("Ctrl+,", "Settings"),
                    ("Ctrl+Q", "Quit"),
                ],
            ),
        ];
        let cols = GROUPS.iter().map(|(title, keys)| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::faint())
                        .child(title.to_uppercase()),
                )
                .children(keys.iter().map(|(k, what)| {
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().text_color(theme::text_soft()).child(*what))
                        .child(div().text_xs().text_color(theme::accent()).child(*k))
                }))
        });
        let content = div().flex().gap_5().children(cols).into_any_element();
        let mut card = self.dialog_card(
            "keyboard",
            theme::accent(),
            "Keyboard shortcuts".into(),
            None,
            content,
            vec![
                button("d-done", "Close", None, true)
                    .on_click(cx.listener(|this, _, w, cx| this.close_dialog(w, cx)))
                    .into_any_element(),
            ],
        );
        card = div().w(px(720.)).child(card).into_any_element();
        card
    }

    fn dialog_card(
        &self,
        icon_name: &'static str,
        accent: Hsla,
        title: SharedString,
        subtitle: Option<SharedString>,
        content: AnyElement,
        buttons: Vec<AnyElement>,
    ) -> AnyElement {
        div()
            .id("dialog")
            .track_focus(&self.dialog_focus)
            .min_w(px(480.))
            .max_w(px(720.))
            .max_h(px(720.))
            .p_5()
            .rounded_xl()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border_strong())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .size(px(38.))
                            .flex_none()
                            .rounded_lg()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(accent.opacity(0.15))
                            .child(icon_el(icon_name, accent, 18.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .children(
                                subtitle
                                    .map(|s| div().text_sm().text_color(theme::muted()).child(s)),
                            ),
                    ),
            )
            .child(content)
            .child(div().flex().justify_end().gap_2().children(buttons))
            .into_any_element()
    }
}

impl Editor {
    /// The fields on screen, in Tab order.
    fn fields(&self, c: &Connection) -> Vec<Entity<TextInput>> {
        let mut v = vec![self.name.clone()];
        match c.protocol {
            Protocol::Sftp => {
                v.extend([self.host.clone(), self.port.clone(), self.username.clone()]);
            }
            Protocol::S3 => {
                v.extend([self.bucket.clone(), self.region.clone(), self.host.clone()]);
            }
        }
        match c.auth {
            Auth::KeyFile { .. } => v.push(self.key_path.clone()),
            Auth::AwsProfile { .. } => v.push(self.profile.clone()),
            Auth::AwsKeys { .. } => v.push(self.access_key.clone()),
            _ => {}
        }
        if ferry::secrets::takes_secret(&c.auth) {
            v.push(self.secret.clone());
        }
        v.push(self.initial_path.clone());
        v
    }
}

/// Why a name can't be used, if it can't.
fn name_problem(name: &str, original: Option<&str>, taken: impl Fn(&str) -> bool) -> Option<String> {
    if name.is_empty() {
        return Some("A name is required".into());
    }
    if name == "." || name == ".." {
        return Some("That name is reserved".into());
    }
    if name.contains('/') {
        return Some("Names can't contain “/”".into());
    }
    if Some(name) != original && taken(name) {
        return Some(format!("“{name}” already exists here"));
    }
    None
}

fn labeled(label: &'static str, field: AnyElement) -> AnyElement {
    labeled_el(label, field)
}

fn labeled_el(label: &'static str, field: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::muted())
                .child(label),
        )
        .child(field)
        .into_any_element()
}

fn field_box(_label: &str, input: Entity<TextInput>, focused: bool, error: bool) -> AnyElement {
    div()
        .h(px(34.))
        .px_2()
        .flex()
        .items_center()
        .rounded_md()
        .bg(theme::bg())
        .border_1()
        .border_color(if error {
            theme::danger()
        } else if focused {
            theme::accent()
        } else {
            theme::border()
        })
        .child(input)
        .into_any_element()
}

fn danger_button(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(30.))
        .px_3()
        .flex()
        .flex_none()
        .items_center()
        .rounded_md()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .bg(theme::danger())
        .text_color(theme::on_accent())
        .hover(|s| s.opacity(0.9))
        .child(label)
}

fn name_list(names: Vec<(String, bool)>) -> AnyElement {
    let more = names.len().saturating_sub(6);
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_2()
        .rounded_md()
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
        .children(names.into_iter().take(6).map(|(n, dir)| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(theme::text_soft())
                .child(icon_el(
                    if dir { "folder" } else { "file" },
                    theme::faint(),
                    13.,
                ))
                .child(div().truncate().child(n))
        }))
        .when(more > 0, |d| {
            d.child(
                div()
                    .text_xs()
                    .text_color(theme::faint())
                    .child(format!("and {more} more")),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        let taken = |n: &str| n == "docs";
        assert!(name_problem("", None, taken).is_some());
        assert!(name_problem("..", None, taken).is_some());
        assert!(name_problem("a/b", None, taken).is_some());
        assert!(name_problem("docs", None, taken).is_some());
        assert!(name_problem("docs", Some("docs"), taken).is_none());
        assert!(name_problem("new", None, taken).is_none());
    }
}
