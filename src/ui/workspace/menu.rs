//! Right-click menus: rows, the empty part of the list, bookmarks, the tab
//! strip's "+", and the upload button.

use std::rc::Rc;

use ferry::model::connection::{ConnectionId, Protocol};
use gpui::{
    AnyElement, Context, FontWeight, Pixels, Point, SharedString, Window, anchored, deferred, div,
    prelude::*, px,
};

use super::{Phase, ToggleHidden, Workspace};
use crate::ui::theme;
use crate::ui::widgets::icon_el;

pub enum Menu {
    Row {
        position: Point<Pixels>,
    },
    Background {
        position: Point<Pixels>,
    },
    Bookmark {
        position: Point<Pixels>,
        id: ConnectionId,
    },
    OpenBookmark {
        position: Point<Pixels>,
    },
    Upload {
        position: Point<Pixels>,
    },
}

type Run = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

enum Entry {
    Item {
        icon: &'static str,
        label: SharedString,
        shortcut: &'static str,
        danger: bool,
        checked: Option<bool>,
        run: Run,
    },
    Sep,
    Header(SharedString),
}

fn item(
    icon: &'static str,
    label: impl Into<SharedString>,
    shortcut: &'static str,
    run: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Entry {
    Entry::Item {
        icon,
        label: label.into(),
        shortcut,
        danger: false,
        checked: None,
        run: Rc::new(run),
    }
}

fn danger(e: Entry) -> Entry {
    match e {
        Entry::Item {
            icon,
            label,
            shortcut,
            checked,
            run,
            ..
        } => Entry::Item {
            icon,
            label,
            shortcut,
            danger: true,
            checked,
            run,
        },
        other => other,
    }
}

fn check(
    label: impl Into<SharedString>,
    on: bool,
    run: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Entry {
    Entry::Item {
        icon: "",
        label: label.into(),
        shortcut: "",
        danger: false,
        checked: Some(on),
        run: Rc::new(run),
    }
}

impl Workspace {
    fn row_menu(&self) -> Vec<Entry> {
        let Some(tab) = self.ready_tab() else {
            return Vec::new();
        };
        let chosen = self.chosen();
        let mut v = Vec::new();
        if chosen.len() == 1 {
            let is_dir = chosen[0].1;
            let row = tab.browser.cursor;
            v.push(item(
                if is_dir { "folder-open" } else { "external" },
                if is_dir {
                    "Open"
                } else {
                    "Open with default app"
                },
                "Enter",
                move |this, _, cx| this.open_row(row, cx),
            ));
            v.push(Entry::Sep);
        }
        let what = if chosen.len() > 1 {
            format!(" {} items", chosen.len())
        } else {
            String::new()
        };
        v.push(item(
            "download",
            format!("Download{what}"),
            "Ctrl+D",
            |this, _, cx| this.download(None, cx),
        ));
        v.push(item(
            "folder-search",
            "Download to…",
            "Ctrl+Shift+D",
            |this, _, cx| this.download_to(cx),
        ));
        v.push(Entry::Sep);
        if chosen.len() == 1 {
            v.push(item("pencil", "Rename…", "F2", |this, w, cx| {
                this.ask_rename(w, cx)
            }));
        }
        v.push(item(
            "link-2",
            if chosen.len() > 1 {
                "Copy URLs"
            } else {
                "Copy URL"
            },
            "Ctrl+Shift+C",
            |this, _, cx| this.copy_url(cx),
        ));
        v.push(Entry::Sep);
        v.push(danger(item(
            "trash",
            format!("Delete{what}…"),
            "Delete",
            |this, w, cx| this.ask_delete(w, cx),
        )));
        v
    }

    fn background_menu(&self) -> Vec<Entry> {
        let hidden = self.settings.show_hidden;
        vec![
            item("upload", "Upload files…", "Ctrl+U", |this, _, cx| {
                this.pick_uploads(false, cx)
            }),
            item(
                "folder-plus",
                "Upload folder…",
                "Ctrl+Shift+U",
                |this, _, cx| this.pick_uploads(true, cx),
            ),
            item(
                "folder-plus",
                "New folder…",
                "Ctrl+Shift+N",
                |this, w, cx| this.ask_new_folder(w, cx),
            ),
            Entry::Sep,
            item("refresh", "Refresh", "F5", |this, w, cx| {
                this.refresh_action(&super::Refresh, w, cx)
            }),
            item("link-2", "Copy URL of this folder", "", |this, _, cx| {
                if let Some(tab) = this.tabs.get_mut(this.active) {
                    tab.browser.clear_selection();
                }
                this.copy_url(cx)
            }),
            Entry::Sep,
            check("Show hidden files", hidden, |this, w, cx| {
                this.toggle_hidden(&ToggleHidden, w, cx)
            }),
        ]
    }

    fn bookmark_menu(&self, id: ConnectionId) -> Vec<Entry> {
        let open = self
            .tabs
            .iter()
            .position(|t| t.connection.id == id && !matches!(t.phase, Phase::Failed(_)));
        let mut v = vec![item(
            "plug",
            if open.is_some() { "Show" } else { "Connect" },
            "",
            move |this, w, cx| this.open_connection(id, w, cx),
        )];
        if let Some(ix) = open {
            v.push(item("unplug", "Disconnect", "", move |this, w, cx| {
                this.close_tab(ix, w, cx)
            }));
        }
        v.push(Entry::Sep);
        v.push(item("pencil", "Edit…", "", move |this, w, cx| {
            this.edit_connection(Some(id), Protocol::Sftp, w, cx)
        }));
        v.push(item("duplicate", "Duplicate…", "", move |this, w, cx| {
            this.duplicate_connection(id, w, cx)
        }));
        v.push(Entry::Sep);
        v.push(danger(item(
            "trash",
            "Delete bookmark…",
            "",
            move |this, w, cx| this.ask_remove_bookmark(id, w, cx),
        )));
        v
    }

    fn open_bookmark_menu(&self) -> Vec<Entry> {
        let mut v = vec![Entry::Header("Open a bookmark".into())];
        for c in self.state.connections() {
            let id = c.id;
            v.push(item(
                super::chrome::protocol_icon(c.protocol),
                c.name.clone(),
                "",
                move |this, w, cx| this.open_connection(id, w, cx),
            ));
        }
        v.push(Entry::Sep);
        v.push(item(
            "plus",
            "New connection…",
            "Ctrl+N",
            |this, w, cx| this.edit_connection(None, Protocol::Sftp, w, cx),
        ));
        v
    }

    fn upload_menu(&self) -> Vec<Entry> {
        vec![
            item("file", "Upload files…", "Ctrl+U", |this, _, cx| {
                this.pick_uploads(false, cx)
            }),
            item(
                "folder",
                "Upload a folder…",
                "Ctrl+Shift+U",
                |this, _, cx| this.pick_uploads(true, cx),
            ),
        ]
    }

    pub(super) fn render_menu(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (position, entries) = match self.menu.as_ref()? {
            Menu::Row { position } => (*position, self.row_menu()),
            Menu::Background { position } => (*position, self.background_menu()),
            Menu::Bookmark { position, id } => (*position, self.bookmark_menu(*id)),
            Menu::OpenBookmark { position } => (*position, self.open_bookmark_menu()),
            Menu::Upload { position } => (*position, self.upload_menu()),
        };
        if entries.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| match e {
                Entry::Sep => div()
                    .h(px(1.))
                    .mx_1()
                    .my_1()
                    .bg(theme::border())
                    .into_any_element(),
                Entry::Header(t) => div()
                    .px_2()
                    .pt_1()
                    .pb_0p5()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::faint())
                    .truncate()
                    .child(t)
                    .into_any_element(),
                Entry::Item {
                    icon,
                    label,
                    shortcut,
                    danger,
                    checked,
                    run,
                } => {
                    let color = if danger {
                        theme::danger()
                    } else {
                        theme::text()
                    };
                    div()
                        .id(("menu-item", i))
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(5.))
                        .text_sm()
                        .text_color(color)
                        .cursor_pointer()
                        .hover(|s| {
                            if danger {
                                s.bg(theme::danger().opacity(0.14))
                            } else {
                                s.bg(theme::accent_soft())
                            }
                        })
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.menu = None;
                            run(this, w, cx);
                            cx.notify();
                        }))
                        .child(match checked {
                            Some(true) => icon_el("check", theme::accent(), 14.).into_any_element(),
                            Some(false) => div().size(px(14.)).flex_none().into_any_element(),
                            None if icon.is_empty() => {
                                div().size(px(14.)).flex_none().into_any_element()
                            }
                            None => icon_el(
                                icon,
                                if danger {
                                    theme::danger()
                                } else {
                                    theme::muted()
                                },
                                14.,
                            )
                            .into_any_element(),
                        })
                        .child(div().flex_1().truncate().child(label))
                        .when(!shortcut.is_empty(), |d| {
                            d.child(div().text_xs().text_color(theme::faint()).child(shortcut))
                        })
                        .into_any_element()
                }
            })
            .collect();
        Some(
            deferred(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("menu")
                            .occlude()
                            .w(px(260.))
                            .p_1()
                            .rounded_lg()
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border_strong())
                            .shadow_lg()
                            .flex()
                            .flex_col()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }))
                            .children(rows),
                    ),
            )
            .with_priority(2),
        )
    }
}
