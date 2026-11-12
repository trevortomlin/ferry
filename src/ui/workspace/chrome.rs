//! Everything around the file list: the bookmarks sidebar, the tab strip,
//! the toolbar and its breadcrumbs, the status bar, the transfers drawer, the
//! toast, and the welcome screen shown when nothing is open.

use ferry::model::connection::{Connection, ConnectionId, Protocol};
use ferry::model::path;
use ferry::model::transfer::{Direction, State, Transfer};
use gpui::{
    AnyElement, Context, ExternalPaths, Focusable, FontWeight, Hsla, MouseButton, MouseDownEvent,
    SharedString, Window, div, prelude::*, px,
};

use super::{DraggedRows, Menu, Phase, ToastKind, UploadFiles, Workspace, tilde};
use crate::ui::format;
use crate::ui::theme;
use crate::ui::widgets::usage_bar;
use crate::ui::widgets::{self as ui, button, icon_button, icon_el, icon_toggle, tooltip};

const SIDEBAR_W: f32 = 248.;

/// The dot that says how a connection is doing.
fn phase_color(phase: Option<&Phase>) -> Option<Hsla> {
    match phase? {
        Phase::Ready => Some(theme::success()),
        Phase::Connecting | Phase::Unlocking { .. } => Some(theme::warning()),
        Phase::Failed(_) => Some(theme::danger()),
    }
}

pub fn protocol_icon(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Sftp => "server",
        Protocol::S3 => "cloud",
    }
}

fn protocol_hue(protocol: Protocol) -> f32 {
    match protocol {
        Protocol::Sftp => 222.,
        Protocol::S3 => 32.,
    }
}

impl Workspace {
    /// The drawer's height; zero when hidden.
    pub(super) fn transfers_height(&self) -> f32 {
        if self.transfers.is_empty() || !self.settings.show_transfers {
            0.
        } else {
            (self.transfers.all().len().min(4) as f32) * 62. + 42.
        }
    }

    pub(super) fn sidebar_width(&self) -> f32 {
        SIDEBAR_W
    }

    fn phase_of(&self, id: ConnectionId) -> Option<&Phase> {
        self.tabs
            .iter()
            .find(|t| t.connection.id == id)
            .map(|t| &t.phase)
    }

    // ── Sidebar ─────────────────────────────────────────────────────────

    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let connections: Vec<Connection> = self.state.connections().to_vec();
        let active_conn = self.tab().map(|t| t.connection.id);
        let mut list = div()
            .id("bookmarks")
            .flex_1()
            .min_h_0()
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .gap(px(2.))
            .overflow_y_scroll();
        if connections.is_empty() {
            list = list.child(
                div()
                    .mt_2()
                    .mx_1()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(theme::border_strong())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::muted())
                    .child("No bookmarks yet. Add an SFTP server or an S3 bucket to get started.")
                    .child(
                        button("side-new", "New connection", Some("plus"), true).on_click(
                            cx.listener(|this, _, w, cx| {
                                this.edit_connection(None, Protocol::Sftp, w, cx)
                            }),
                        ),
                    ),
            );
        }
        for (i, c) in connections.into_iter().enumerate() {
            let id = c.id;
            let active = active_conn == Some(id);
            let dot = phase_color(self.phase_of(id));
            let hue = protocol_hue(c.protocol);
            list = list.child(
                div()
                    .id(("bookmark", i))
                    .group("bookmark")
                    .px_2()
                    .py(px(7.))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme::accent_soft()))
                    .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
                    .on_click(cx.listener(move |this, _, w, cx| this.open_connection(id, w, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.menu = Some(Menu::Bookmark {
                                position: e.position,
                                id,
                            });
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .size(px(32.))
                            .flex_none()
                            .rounded_lg()
                            .bg(theme::category_soft(hue))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_el(
                                protocol_icon(c.protocol),
                                theme::category(hue),
                                16.,
                            ))
                            .when_some(dot, |d, color| {
                                d.child(
                                    div()
                                        .absolute()
                                        .right(px(-2.))
                                        .bottom(px(-2.))
                                        .size(px(10.))
                                        .rounded_full()
                                        .bg(color)
                                        .border_2()
                                        .border_color(theme::bg()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if active {
                                        theme::text()
                                    } else {
                                        theme::text_soft()
                                    })
                                    .child(if c.name.is_empty() {
                                        c.summary()
                                    } else {
                                        c.name.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::faint())
                                    .child(c.summary()),
                            ),
                    )
                    .child(
                        div()
                            .id(("bookmark-edit", i))
                            .invisible()
                            .group_hover("bookmark", |s| s.visible())
                            .size(px(22.))
                            .flex_none()
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(theme::border()))
                            .tooltip(tooltip("Edit"))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                cx.stop_propagation();
                                this.edit_connection(Some(id), Protocol::Sftp, w, cx)
                            }))
                            .child(icon_el("pencil", theme::muted(), 12.)),
                    ),
            );
        }

        let transfers_on = self.settings.show_transfers;
        let active = self.transfers.active();
        div()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .h(px(56.))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        div()
                            .size(px(30.))
                            .rounded_lg()
                            .bg(theme::brand().opacity(0.16))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_el("ferry", theme::brand(), 19.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::text())
                                    .child("Ferry"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::faint())
                                    .child("SFTP · Amazon S3"),
                            ),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::faint())
                            .child("BOOKMARKS"),
                    )
                    .child(
                        icon_button("add-bookmark", "plus", true)
                            .size(px(24.))
                            .tooltip(tooltip("New connection  ·  Ctrl+N"))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.edit_connection(None, Protocol::Sftp, w, cx)
                            })),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        icon_toggle("side-transfers", "transfers", true, transfers_on)
                            .tooltip(tooltip("Transfers  ·  Ctrl+J"))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.toggle_transfers(&super::ToggleTransfers, w, cx)
                            })),
                    )
                    .when(active > 0, |d| {
                        d.child(
                            div()
                                .px_1p5()
                                .rounded_full()
                                .bg(theme::accent())
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme::on_accent())
                                .child(active.to_string()),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        icon_button("side-help", "keyboard", true)
                            .tooltip(tooltip("Keyboard shortcuts  ·  F1"))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.show_help(&super::ShowHelp, w, cx)
                            })),
                    )
                    .child(
                        icon_button("side-settings", "settings", true)
                            .tooltip(tooltip("Settings  ·  Ctrl+,"))
                            .on_click(cx.listener(|this, _, w, cx| this.open_settings(w, cx))),
                    ),
            )
    }

    // ── Tabs ────────────────────────────────────────────────────────────

    pub(super) fn render_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let active = i == self.active;
                let dot = phase_color(Some(&t.phase)).unwrap_or(theme::faint());
                let title = t.title();
                div()
                    .id(("tab", i))
                    .group("tab")
                    .h(px(30.))
                    .max_w(px(220.))
                    .min_w(px(110.))
                    .pl_2p5()
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_t_lg()
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |d| {
                        d.bg(theme::panel())
                            .text_color(theme::text())
                            .border_1()
                            .border_b_0()
                            .border_color(theme::border())
                    })
                    .when(!active, |d| {
                        d.text_color(theme::muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                    })
                    .on_click(cx.listener(move |this, _, w, cx| this.switch_tab(i, w, cx)))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseDownEvent, w, cx| {
                            this.close_tab(i, w, cx)
                        }),
                    )
                    .child(div().size(px(7.)).flex_none().rounded_full().bg(dot))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .id(("tab-close", i))
                            .size(px(20.))
                            .flex_none()
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!active, |d| {
                                d.invisible().group_hover("tab", |s| s.visible())
                            })
                            .hover(|s| s.bg(theme::border()))
                            .tooltip(tooltip("Disconnect  ·  Ctrl+W"))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, w, cx)
                            }))
                            .child(icon_el("x", theme::muted(), 12.)),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex_none()
            .h(px(38.))
            .px_2()
            .pt(px(8.))
            .flex()
            .items_end()
            .gap_1()
            .bg(theme::bg())
            .border_b_1()
            .border_color(theme::border())
            .children(tabs)
            .child(
                div()
                    .id("tab-add")
                    .mb(px(3.))
                    .size(px(24.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .tooltip(tooltip("Open a bookmark"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            this.menu = Some(Menu::OpenBookmark {
                                position: e.position,
                            });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(icon_el("plus", theme::muted(), 14.)),
            )
    }

    // ── Toolbar ─────────────────────────────────────────────────────────

    pub(super) fn render_toolbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (can_back, can_fwd, can_up, ready) = self
            .tab()
            .map(|t| {
                let ready = t.phase == Phase::Ready;
                (
                    t.can_back() && ready,
                    t.can_forward() && ready,
                    t.can_up(),
                    ready,
                )
            })
            .unwrap_or((false, false, false, false));
        let has_selection = self
            .tab()
            .is_some_and(|t| !t.browser.selection().is_empty());
        let hidden = self.settings.show_hidden;
        let filter_focused = self.filter_input.focus_handle(cx).is_focused(window);
        let filter_text = self.filter_input.read(cx).text().to_string();

        div()
            .flex_none()
            .h(px(50.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::border())
            .child(
                icon_button("back", "arrow-left", can_back)
                    .tooltip(tooltip("Back  ·  Alt+Left"))
                    .when(can_back, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))
                    }),
            )
            .child(
                icon_button("fwd", "arrow-right", can_fwd)
                    .tooltip(tooltip("Forward  ·  Alt+Right"))
                    .when(can_fwd, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.go_forward(cx)))
                    }),
            )
            .child(
                icon_button("up", "arrow-up", can_up)
                    .tooltip(tooltip("Enclosing folder  ·  Backspace"))
                    .when(can_up, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.go_up(cx)))
                    }),
            )
            .child(self.render_location(window, cx))
            .child(
                div()
                    .id("filter")
                    .key_context("FilterBar")
                    .w(px(200.))
                    .flex_none()
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .bg(theme::bg())
                    .border_1()
                    .border_color(if filter_focused {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .when(!ready, |d| d.opacity(0.5))
                    .cursor_text()
                    .on_click(
                        cx.listener(|this, _, w, cx| this.focus_filter(&super::FocusFilter, w, cx)),
                    )
                    .child(icon_el(
                        if filter_text.is_empty() {
                            "search"
                        } else {
                            "filter"
                        },
                        if filter_text.is_empty() {
                            theme::faint()
                        } else {
                            theme::accent()
                        },
                        14.,
                    ))
                    .child(div().flex_1().min_w_0().child(self.filter_input.clone()))
                    .when(!filter_text.is_empty(), |d| {
                        d.child(
                            div()
                                .id("filter-clear")
                                .size(px(18.))
                                .rounded(px(4.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::border()))
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.filter_input.update(cx, |i, cx| i.clear(w, cx));
                                    this.focus_list(w, cx);
                                }))
                                .child(icon_el("x", theme::muted(), 12.)),
                        )
                    })
                    .when(filter_text.is_empty() && !filter_focused, |d| {
                        d.child(ui::kbd("Ctrl+F"))
                    }),
            )
            .child(div().w(px(4.)))
            .child(
                icon_button("refresh", "refresh", ready)
                    .tooltip(tooltip("Refresh  ·  F5"))
                    .when(ready, |b| {
                        b.on_click(
                            cx.listener(|this, _, w, cx| {
                                this.refresh_action(&super::Refresh, w, cx)
                            }),
                        )
                    }),
            )
            .child(
                icon_button("new-folder", "folder-plus", ready)
                    .tooltip(tooltip("New folder  ·  Ctrl+Shift+N"))
                    .when(ready, |b| {
                        b.on_click(cx.listener(|this, _, w, cx| this.ask_new_folder(w, cx)))
                    }),
            )
            .child(
                icon_toggle(
                    "hidden",
                    if hidden { "eye" } else { "eye-off" },
                    true,
                    hidden,
                )
                .tooltip(tooltip("Hidden files  ·  Ctrl+H"))
                .on_click(
                    cx.listener(|this, _, w, cx| this.toggle_hidden(&super::ToggleHidden, w, cx)),
                ),
            )
            .child(div().w(px(6.)))
            .child(
                button("download", "Download", Some("download"), false)
                    .when(!(ready && has_selection), |b| b.opacity(0.45))
                    .tooltip(tooltip("Download the selection  ·  Ctrl+D"))
                    .when(ready && has_selection, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.download(None, cx)))
                    }),
            )
            .child(
                button("upload", "Upload", Some("upload"), true)
                    .when(!ready, |b| b.opacity(0.45))
                    .tooltip(tooltip("Upload files  ·  Ctrl+U  (or drop them here)"))
                    .when(ready, |b| {
                        b.on_click(
                            cx.listener(|this, _, w, cx| this.upload_files(&UploadFiles, w, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                this.menu = Some(Menu::Upload {
                                    position: e.position,
                                });
                                cx.notify();
                            }),
                        )
                    }),
            )
    }

    /// Breadcrumbs, or a path field while editing (Ctrl+L).
    fn render_location(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.editing_location {
            let focused = self.location_input.focus_handle(cx).is_focused(window);
            return div()
                .id("location")
                .key_context("LocationBar")
                .flex_1()
                .min_w_0()
                .mx_1()
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .bg(theme::bg())
                .border_1()
                .border_color(if focused {
                    theme::accent()
                } else {
                    theme::border()
                })
                .child(icon_el("folder", theme::faint(), 14.))
                .child(div().flex_1().min_w_0().child(self.location_input.clone()))
                .child(ui::kbd("Enter"))
                .into_any_element();
        }
        let Some(tab) = self.tab() else {
            return div().flex_1().into_any_element();
        };
        let ready = tab.phase == Phase::Ready;
        let current = tab.browser.path.clone();
        let root_label = match tab.connection.protocol {
            Protocol::Sftp => tab.connection.host.clone(),
            Protocol::S3 => tab.connection.bucket.clone(),
        };
        let mut crumbs: Vec<(String, String)> = path::crumbs(&current)
            .map(|(name, full)| (name.to_owned(), full.to_owned()))
            .collect();
        // Deep paths collapse in the middle.
        let collapsed = crumbs.len() > 5;
        if collapsed {
            let tail = crumbs.split_off(crumbs.len() - 3);
            crumbs.truncate(1);
            crumbs.push(("…".into(), String::new()));
            crumbs.extend(tail);
        }
        let mut bar = div()
            .id("crumbs")
            .flex_1()
            .min_w_0()
            .mx_1()
            .h(px(32.))
            .px_1()
            .flex()
            .items_center()
            .gap_0p5()
            .rounded_md()
            .overflow_hidden()
            .bg(theme::bg())
            .border_1()
            .border_color(theme::border())
            .cursor_text()
            .on_click(
                cx.listener(|this, _, w, cx| this.edit_location(&super::EditLocation, w, cx)),
            );
        let protocol = tab.connection.protocol;
        bar = bar.child(self.crumb(
            "crumb-root".into(),
            root_label,
            "/".into(),
            Some(super::chrome::protocol_icon(protocol)),
            current == "/",
            ready,
            cx,
        ));
        let n = crumbs.len();
        for (i, (name, full)) in crumbs.into_iter().enumerate() {
            bar = bar.child(icon_el("chevron-right", theme::faint(), 12.));
            if full.is_empty() {
                bar = bar.child(div().px_1().text_color(theme::faint()).child(name));
                continue;
            }
            let last = i + 1 == n;
            bar = bar.child(self.crumb(
                SharedString::from(format!("crumb-{i}")),
                name,
                full,
                None,
                last,
                ready,
                cx,
            ));
        }
        if tab.browser.loading && ready {
            bar = bar
                .child(div().flex_1())
                .child(div().pr_2().child(ui::busy_dots_small("crumb-busy")));
        }
        bar.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn crumb(
        &self,
        id: SharedString,
        label: String,
        target: String,
        icon_name: Option<&'static str>,
        current: bool,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let go = target.clone();
        let drop_rows = target.clone();
        let drop_files = target;
        div()
            .id(gpui::ElementId::Name(id))
            .h(px(24.))
            .px_1p5()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .rounded(px(5.))
            .text_sm()
            .cursor_pointer()
            .when(current, |d| {
                d.text_color(theme::text()).font_weight(FontWeight::MEDIUM)
            })
            .when(!current, |d| {
                d.text_color(theme::muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
            })
            .when_some(icon_name, |d, i| d.child(icon_el(i, theme::accent(), 13.)))
            .child(div().max_w(px(220.)).truncate().child(label))
            .when(ready, |d| {
                d.on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.go(go.clone(), cx);
                }))
                .drag_over::<DraggedRows>(|s, _, _, _| {
                    s.bg(theme::accent_strong()).text_color(theme::text())
                })
                .drag_over::<ExternalPaths>(|s, _, _, _| {
                    s.bg(theme::accent_strong()).text_color(theme::text())
                })
                .on_drop(cx.listener(move |this, d: &DraggedRows, _, cx| {
                    if let Some(paths) = this.dragged_paths(d) {
                        this.move_into(paths, drop_rows.clone(), cx);
                    }
                }))
                .on_drop(cx.listener(move |this, d: &ExternalPaths, _, cx| {
                    this.upload_paths(d.paths().to_vec(), Some(drop_files.clone()), cx);
                }))
            })
            .into_any_element()
    }

    // ── Status bar ──────────────────────────────────────────────────────

    pub(super) fn render_status(&mut self, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = div()
            .flex_none()
            .h(px(28.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .bg(theme::panel())
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::faint());
        let Some(tab) = self.tab() else {
            return bar;
        };
        if tab.phase == Phase::Ready {
            let b = &tab.browser;
            let (visible, total, selected) = (b.visible(), b.total(), b.selection().len());
            let summary = if b.loading {
                format!("{} so far…", format::items(total as u64))
            } else if selected > 0 {
                format!(
                    "{} of {} selected",
                    format::count(selected as u64),
                    format::count(visible as u64)
                )
            } else {
                format::items(visible as u64)
            };
            bar = bar.child(div().text_color(theme::muted()).child(summary));
            if !b.loading && visible < total {
                let hidden_by = if tab.filter.is_empty() {
                    "hidden"
                } else {
                    "filtered out"
                };
                bar = bar.child(format!(
                    "{} {hidden_by}",
                    format::count((total - visible) as u64)
                ));
            }
            if let Some(t) = b.load_time {
                bar = bar.child(format!("listed in {}", fmt_duration(t)));
            }
        } else {
            bar = bar.child(tab.connection.summary());
        }
        bar = bar.child(div().flex_1());
        if let Some((done, total)) = self.transfers.overall() {
            let active = self.transfers.active();
            bar = bar
                .child(ui::busy_dots_small("status-busy"))
                .child(format!(
                    "{} · {} of {}",
                    format::plural(active as u64, "transfer", "transfers"),
                    format::bytes(done),
                    format::bytes(total)
                ))
                .child(div().w(px(80.)).child(usage_bar(
                    done as f32 / total.max(1) as f32,
                    theme::accent(),
                    4.,
                )));
        }
        let failed = self.transfers.failed();
        if failed > 0 {
            bar = bar.child(
                div()
                    .text_color(theme::danger())
                    .child(format!("{failed} failed")),
            );
        }
        if tab.phase == Phase::Ready {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(icon_el("lock", theme::faint(), 11.))
                    .child(tab.connection.summary()),
            );
        }
        bar
    }

    // ── Transfers drawer ────────────────────────────────────────────────

    pub(super) fn render_transfers(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.transfers.is_empty() || !self.settings.show_transfers {
            return None;
        }
        let all: Vec<Transfer> = self.transfers.all().to_vec();
        let active = self.transfers.active();
        let finished = all.len() - active;
        let rate: f64 = all
            .iter()
            .filter(|t| t.state == State::Running)
            .filter_map(Transfer::rate)
            .sum();
        let rows: Vec<AnyElement> = all
            .iter()
            .enumerate()
            .map(|(i, t)| self.transfer_row(i, t, cx))
            .collect();
        let height = self.transfers_height();
        Some(
            div()
                .flex_none()
                .h(px(height))
                .flex()
                .flex_col()
                .bg(theme::bg())
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex_none()
                        .h(px(36.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon_el("transfers", theme::accent(), 14.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Transfers"),
                        )
                        .child(div().text_xs().text_color(theme::faint()).child(
                            match (active, finished) {
                                (0, f) => format!("{f} finished"),
                                (a, 0) => format!("{a} in progress"),
                                (a, f) => format!("{a} in progress · {f} finished"),
                            },
                        ))
                        .when(rate > 0., |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(format::rate(rate)),
                            )
                        })
                        .child(div().flex_1())
                        .when(finished > 0, |d| {
                            d.child(
                                div()
                                    .id("clear-finished")
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.transfers.clear_finished();
                                        cx.notify();
                                    }))
                                    .child("Clear finished"),
                            )
                        })
                        .child(
                            icon_button("hide-transfers", "chevron-down", true)
                                .size(px(24.))
                                .tooltip(tooltip("Hide  ·  Ctrl+J"))
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.toggle_transfers(&super::ToggleTransfers, w, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .id("transfer-list")
                        .flex_1()
                        .min_h_0()
                        .px_2()
                        .pb_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .overflow_y_scroll()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn transfer_row(&self, i: usize, t: &Transfer, cx: &mut Context<Self>) -> AnyElement {
        let id = t.id;
        let (dir_icon, dir_color) = match t.direction {
            Direction::Download => ("download", theme::accent()),
            Direction::Upload => ("upload", theme::success()),
        };
        let opening = self.open_after.contains(&id);
        let where_to: String = match t.direction {
            Direction::Download if opening => "Opening".into(),
            Direction::Download => format!(
                "to {}",
                tilde(t.local.parent().unwrap_or(std::path::Path::new("/")))
            ),
            Direction::Upload => format!("to {}", path::parent(&t.remote).unwrap_or("/")),
        };
        let (detail, detail_color): (String, Hsla) = match &t.state {
            State::Queued => ("Waiting…".into(), theme::faint()),
            State::Probing => ("Checking the destination…".into(), theme::faint()),
            State::Blocked(_) => ("Already exists — waiting for you".into(), theme::warning()),
            State::Running => {
                let mut s = if t.total > 0 {
                    format!("{} of {}", format::bytes(t.done), format::bytes(t.total))
                } else {
                    format::bytes(t.done)
                };
                if let Some(rate) = t.rate() {
                    s.push_str(&format!(" · {}", format::rate(rate)));
                }
                if let Some(left) = t.remaining() {
                    s.push_str(&format!(" · {} left", format::eta(left.as_secs_f64())));
                }
                (s, theme::muted())
            }
            State::Done => (
                if t.total > 0 {
                    format!("Done · {}", format::bytes(t.total))
                } else {
                    "Done".into()
                },
                theme::success(),
            ),
            State::Cancelled => ("Cancelled".into(), theme::faint()),
            State::Failed(why) => (why.clone(), theme::danger()),
        };
        let fraction = t.fraction();
        let running = t.state == State::Running;
        let finished = t.state.is_finished();
        let failed = matches!(t.state, State::Failed(_));
        let done_download = t.state == State::Done && t.direction == Direction::Download;
        let local = t.local.clone();
        div()
            .id(("transfer", i))
            .group("transfer")
            .h(px(60.))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded_lg()
            .hover(|s| s.bg(theme::hover()))
            .child(
                div()
                    .size(px(30.))
                    .flex_none()
                    .rounded_lg()
                    .bg(dir_color.opacity(0.13))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_el(dir_icon, dir_color, 15.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(relative_name_width())
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme::text())
                                    .child(t.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::faint())
                                    .child(where_to),
                            ),
                    )
                    .child(match (running || !finished, fraction) {
                        (true, Some(f)) => usage_bar(f, theme::accent(), 4.).into_any_element(),
                        (true, None) => {
                            usage_bar(0.08, theme::accent().opacity(0.5), 4.).into_any_element()
                        }
                        (false, _) => div().into_any_element(),
                    })
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(detail_color)
                            .child(detail),
                    ),
            )
            .when(done_download && !opening, |d| {
                d.child(
                    icon_button(("transfer-reveal", i), "folder-open", true)
                        .size(px(26.))
                        .tooltip(tooltip("Show in folder"))
                        .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&local))),
                )
            })
            .when(failed, |d| {
                d.child(
                    icon_button(("transfer-retry", i), "refresh", true)
                        .size(px(26.))
                        .tooltip(tooltip("Try again"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.retry_transfer(id);
                            cx.notify();
                        })),
                )
            })
            .child(
                icon_button(("transfer-x", i), "x", true)
                    .size(px(26.))
                    .tooltip(tooltip(if finished { "Remove" } else { "Cancel" }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if finished {
                            this.transfers.remove(id);
                        } else {
                            this.cancel_transfer(id);
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    // ── Toast ───────────────────────────────────────────────────────────

    pub(super) fn render_toast(&mut self, _cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let t = self.toast.as_ref()?;
        let (color, icon_name) = match t.kind {
            ToastKind::Error => (theme::danger(), "alert"),
            ToastKind::Success => (theme::success(), "check"),
            ToastKind::Info => (theme::accent(), "info"),
        };
        Some(
            div()
                .absolute()
                .bottom(px(44.))
                .left(px(SIDEBAR_W))
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("toast")
                        .max_w(px(640.))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(theme::raised())
                        .border_1()
                        .border_color(color.opacity(0.45))
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(icon_el(icon_name, color, 14.))
                        .child(div().min_w_0().truncate().child(t.text.clone())),
                ),
        )
    }

    // ── Welcome ─────────────────────────────────────────────────────────

    pub(super) fn render_welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let connections: Vec<Connection> =
            self.state.connections().iter().take(6).cloned().collect();
        let cards: Vec<AnyElement> = connections
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let id = c.id;
                let hue = protocol_hue(c.protocol);
                div()
                    .id(("welcome-card", i))
                    .w(px(232.))
                    .p_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_xl()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::accent()).bg(theme::raised()))
                    .on_click(cx.listener(move |this, _, w, cx| this.open_connection(id, w, cx)))
                    .child(
                        div()
                            .size(px(36.))
                            .flex_none()
                            .rounded_lg()
                            .bg(theme::category_soft(hue))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_el(
                                protocol_icon(c.protocol),
                                theme::category(hue),
                                18.,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(c.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme::faint())
                                    .child(c.summary()),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let has_cards = !cards.is_empty();
        div()
            .id("welcome")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_5()
            .bg(theme::panel())
            .px_8()
            .child(
                div()
                    .size(px(76.))
                    .rounded(px(22.))
                    .bg(theme::brand().opacity(0.14))
                    .border_1()
                    .border_color(theme::brand().opacity(0.3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_el("ferry", theme::brand(), 44.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .child("Connect to a server"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child(if has_cards {
                                "Pick a bookmark, or add a new SFTP server or S3 bucket."
                            } else {
                                "Add an SFTP server or an Amazon S3 bucket to start browsing."
                            }),
                    ),
            )
            .when(has_cards, |d| {
                d.child(
                    div()
                        .max_w(px(740.))
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_3()
                        .children(cards),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "welcome-sftp",
                            "New SFTP connection",
                            Some("server"),
                            !has_cards,
                        )
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.edit_connection(None, Protocol::Sftp, w, cx)
                        })),
                    )
                    .child(
                        button("welcome-s3", "New S3 connection", Some("cloud"), false).on_click(
                            cx.listener(|this, _, w, cx| {
                                this.edit_connection(None, Protocol::S3, w, cx)
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::faint())
                    .child(ui::kbd("Ctrl+N"))
                    .child("new connection")
                    .child(div().w(px(8.)))
                    .child(ui::kbd("F1"))
                    .child("keyboard shortcuts"),
            )
    }
}

fn relative_name_width() -> gpui::DefiniteLength {
    gpui::relative(0.6)
}

pub fn fmt_duration(d: std::time::Duration) -> String {
    let us = d.as_micros();
    if us < 1000 {
        format!("{us} µs")
    } else if us < 100_000 {
        format!("{:.1} ms", us as f64 / 1000.)
    } else {
        format!("{:.2} s", d.as_secs_f64())
    }
}
