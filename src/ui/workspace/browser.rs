//! The file list: keyboard and mouse handling, the header, the rows (painted
//! directly, one layout node each), and the states around an empty list —
//! connecting, failed, loading, empty, filtered to nothing.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ferry::model::browser::Move;
use ferry::model::entries::Kind;
use ferry::model::order::SortKey;
use ferry::model::path;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Element, ElementId, ExternalPaths, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, Pixels, Render, ScrollStrategy, SharedString, Style, Window, actions, canvas,
    div, point, prelude::*, px, relative, size, uniform_list,
};

use super::{Workspace, DraggedRows, Menu, Phase, Tab};
use crate::ui::assets::icon;
use crate::ui::filetype::{self, Category};
use crate::ui::format::{self, DateFormatter};
use crate::ui::theme;
use crate::ui::widgets::{self as ui, Align, TextPainter, icon_el};

actions!(
    file_list,
    [
        MoveUp,
        MoveDown,
        PageUp,
        PageDown,
        MoveHome,
        MoveEnd,
        ExtendUp,
        ExtendDown,
        ExtendHome,
        ExtendEnd,
        Open,
        GoUp,
        GoBack,
        GoForward,
        SelectAll,
        ClearSelection,
        Refresh,
    ]
);

pub const ROW_H: Pixels = px(30.);
const HEADER_H: Pixels = px(30.);
const NAME_MIN: Pixels = px(180.);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    Size,
    Modified,
    Kind,
    Permissions,
}

impl Col {
    fn width(self) -> Option<Pixels> {
        match self {
            Col::Name => None,
            Col::Size => Some(px(96.)),
            Col::Modified => Some(px(150.)),
            Col::Kind => Some(px(140.)),
            Col::Permissions => Some(px(104.)),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Size => "Size",
            Col::Modified => "Modified",
            Col::Kind => "Kind",
            Col::Permissions => "Permissions",
        }
    }
    fn sort_key(self) -> Option<SortKey> {
        match self {
            Col::Name => Some(SortKey::Name),
            Col::Size => Some(SortKey::Size),
            Col::Modified => Some(SortKey::Modified),
            _ => None,
        }
    }
    fn align(self) -> Align {
        match self {
            Col::Size => Align::Right,
            _ => Align::Left,
        }
    }
}

/// x offset and width of each column within a row `width` wide.
fn column_spans(width: Pixels, cols: &[Col]) -> Vec<(Pixels, Pixels)> {
    let fixed: Pixels = cols.iter().filter_map(|c| c.width()).sum();
    let mut x = px(0.);
    cols.iter()
        .map(|c| {
            let w = c.width().unwrap_or_else(|| (width - fixed).max(NAME_MIN));
            let span = (x, w);
            x += w;
            span
        })
        .collect()
}

/// What a row looks like, from its listing entry.
pub fn describe(kind: Kind, name: &str, mode: u32) -> (Category, &'static str) {
    match kind {
        Kind::Directory => (Category::Folder, "Folder"),
        Kind::Symlink => (Category::Other, "Link"),
        Kind::File => filetype::classify(name, mode & 0o111 != 0),
    }
}

impl Workspace {
    // ── Keyboard ────────────────────────────────────────────────────────

    fn with_tab(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Tab, isize)) {
        let page = (self.list_height.get() / ROW_H).floor().max(1.) as isize;
        if let Some(tab) = self.tabs.get_mut(self.active)
            && tab.phase == Phase::Ready
        {
            f(tab, page);
            if tab.browser.scroll_to_cursor {
                tab.browser.scroll_to_cursor = false;
                tab.scroll
                    .scroll_to_item(tab.browser.cursor, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(-1, Move::Replace));
    }
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| {
            if t.browser.selection().is_empty() && t.browser.cursor == 0 {
                // The first press lands on the first row rather than skipping it.
                t.browser.move_cursor(0, Move::Replace)
            } else {
                t.browser.move_cursor(1, Move::Replace)
            }
        });
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, page| t.browser.move_cursor(-page, Move::Replace));
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, page| t.browser.move_cursor(page, Move::Replace));
    }
    fn move_home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(isize::MIN / 2, Move::Replace));
    }
    fn move_end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(isize::MAX / 2, Move::Replace));
    }
    fn extend_up(&mut self, _: &ExtendUp, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(-1, Move::Extend));
    }
    fn extend_down(&mut self, _: &ExtendDown, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(1, Move::Extend));
    }
    fn extend_home(&mut self, _: &ExtendHome, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(isize::MIN / 2, Move::Extend));
    }
    fn extend_end(&mut self, _: &ExtendEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.move_cursor(isize::MAX / 2, Move::Extend));
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, _| t.browser.select_all());
    }
    fn clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        let has = self
            .tab()
            .is_some_and(|t| !t.browser.selection().is_empty());
        if has {
            self.with_tab(cx, |t, _| t.browser.clear_selection());
        } else {
            cx.propagate();
        }
    }
    fn open_action(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.ready_tab() else {
            return;
        };
        let rows: Vec<usize> = {
            let selected = tab.browser.selection();
            if selected.len() > 1 {
                // Several files: open each. Several folders make no sense.
                (0..tab.browser.visible())
                    .filter(|&r| tab.browser.is_selected(r) && !tab.browser.is_dir(r))
                    .take(20)
                    .collect()
            } else {
                vec![tab.browser.cursor]
            }
        };
        for row in rows {
            self.open_row(row, cx);
        }
    }

    /// Typing a name jumps to it; letters typed within a moment add up.
    fn on_list_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = e.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        let Some(ch) = e.keystroke.key_char.as_deref() else {
            return;
        };
        if ch.is_empty() || ch.chars().any(char::is_control) {
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let fresh = tab.typeahead.1.elapsed() > Duration::from_millis(900);
        if fresh {
            tab.typeahead.0.clear();
        }
        tab.typeahead.0.push_str(ch);
        tab.typeahead.1 = Instant::now();
        let typed = tab.typeahead.0.clone();
        let first = typed.chars().next().unwrap_or(' ');
        // Repeating one letter steps through names starting with it.
        let (needle, from) = if typed.chars().count() > 1 && typed.chars().all(|c| c == first) {
            (first.to_string(), tab.browser.cursor + 1)
        } else {
            (typed, tab.browser.cursor)
        };
        if let Some(row) = tab.browser.find_prefix(from, &needle) {
            tab.browser.focus(row, Move::Replace);
            tab.scroll.scroll_to_item(row, ScrollStrategy::Center);
            cx.notify();
        }
        cx.stop_propagation();
    }

    // ── Mouse ───────────────────────────────────────────────────────────

    fn row_mouse_down(
        &mut self,
        row: usize,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.list_focus, cx);
        self.menu = None;
        let m = e.modifiers;
        if let Some(tab) = self.tabs.get_mut(self.active)
            && !m.control
            && !m.shift
            && !tab.browser.is_selected(row)
        {
            tab.browser.focus(row, Move::Replace);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn row_click(&mut self, row: usize, e: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let m = e.modifiers();
        if e.click_count() >= 2 && !m.control && !m.shift {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.browser.focus(row, Move::Replace);
            }
            self.open_row(row, cx);
            return;
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let how = if m.control {
                Move::Toggle
            } else if m.shift {
                Move::Extend
            } else {
                Move::Replace
            };
            tab.browser.focus(row, how);
        }
        cx.notify();
    }

    fn row_right_down(
        &mut self,
        row: usize,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.list_focus, cx);
        if let Some(tab) = self.tabs.get_mut(self.active)
            && !tab.browser.is_selected(row)
        {
            tab.browser.focus(row, Move::Replace);
        }
        self.menu = Some(Menu::Row {
            position: e.position,
        });
        cx.notify();
        cx.stop_propagation();
    }

    fn background_down(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus, cx);
        self.menu = None;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.browser.clear_selection();
        }
        cx.notify();
    }

    fn background_right_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.list_focus, cx);
        if self.ready_tab().is_some() {
            self.menu = Some(Menu::Background {
                position: e.position,
            });
        }
        cx.notify();
    }

    // ── Rendering ───────────────────────────────────────────────────────

    fn columns(&self, width: Pixels) -> Vec<Col> {
        let mut cols = vec![
            Col::Name,
            Col::Size,
            Col::Modified,
            Col::Kind,
            Col::Permissions,
        ];
        // S3 objects carry no permission bits.
        if self.tab().is_some_and(|t| {
            t.connection.protocol == ferry::model::connection::Protocol::S3
        }) {
            cols.retain(|c| *c != Col::Permissions);
        }
        let need = |cols: &Vec<Col>| -> Pixels {
            cols.iter().filter_map(|c| c.width()).sum::<Pixels>() + NAME_MIN + px(24.)
        };
        while cols.len() > 2 && need(&cols) > width {
            cols.pop();
        }
        cols
    }

    pub(super) fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab() else {
            return div().into_any_element();
        };
        let focused = self.list_focus.is_focused(window);
        let has_rows = tab.phase == Phase::Ready && tab.browser.visible() > 0;
        let body = if has_rows {
            self.render_list(focused, window, cx)
        } else {
            self.render_empty(cx)
        };
        let height = self.list_height.clone();
        let entity = cx.entity().downgrade();
        let ready = tab.phase == Phase::Ready;
        div()
            .id("list-area")
            .key_context("FileList")
            .track_focus(&self.list_focus)
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::move_home))
            .on_action(cx.listener(Self::move_end))
            .on_action(cx.listener(Self::extend_up))
            .on_action(cx.listener(Self::extend_down))
            .on_action(cx.listener(Self::extend_home))
            .on_action(cx.listener(Self::extend_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_selection))
            .on_action(cx.listener(Self::open_action))
            .on_key_down(cx.listener(Self::on_list_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::background_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::background_right_down))
            .when(ready, |d| {
                d.drag_over::<ExternalPaths>(|s, _, _, _| {
                    s.bg(theme::accent_soft())
                        .border_2()
                        .border_color(theme::accent())
                })
                .on_drop(cx.listener(|this, d: &ExternalPaths, _, cx| {
                    this.upload_paths(d.paths().to_vec(), None, cx);
                }))
            })
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .child(body)
            // Measures the list so Page Up / Page Down move a screenful.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let h = bounds.size.height - HEADER_H;
                        if (h - height.get()).abs() > px(0.5) {
                            height.set(h);
                            if let Some(e) = entity.upgrade() {
                                cx.notify(e.entity_id());
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
            .into_any_element()
    }

    fn render_header(&self, cols: &[Col], cx: &mut Context<Self>) -> impl IntoElement {
        let sort = self.settings.sort;
        div()
            .flex_none()
            .h(HEADER_H)
            .px_1()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::faint())
            .children(cols.iter().enumerate().map(|(i, &col)| {
                let active = col.sort_key() == Some(sort.key);
                let arrow = if !active {
                    ""
                } else if sort.descending {
                    "chevron-down"
                } else {
                    "chevron-up"
                };
                let cell = div()
                    .id(("col", i))
                    .h_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(i == 0, |d| d.pl(px(36.)))
                    .when(col.align() == Align::Right, |d| d.justify_end())
                    .when(active, |d| d.text_color(theme::text_soft()))
                    .child(col.label())
                    .when(!arrow.is_empty(), |d| {
                        d.child(icon_el(arrow, theme::accent(), 12.))
                    })
                    .when_some(col.sort_key(), |d, key| {
                        d.cursor_pointer()
                            .hover(|s| s.text_color(theme::text()))
                            .on_click(cx.listener(move |this, _, _, cx| this.sort_by(key, cx)))
                    });
                match col.width() {
                    Some(w) => cell.w(w).flex_none(),
                    None => cell.flex_1().min_w(NAME_MIN),
                }
            }))
    }

    fn render_list(&self, focused: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab() else {
            return div().into_any_element();
        };
        let width = window.viewport_size().width - px(self.sidebar_width());
        let cols = Rc::new(self.columns(width));
        let count = tab.browser.visible();
        let dates = DateFormatter::new(self.settings.clock == ferry::model::settings::Clock::Utc);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_header(&cols, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "rows",
                            count,
                            cx.processor(move |this, range: Range<usize>, _w, cx| {
                                range
                                    .map(|r| this.render_row(r, cols.clone(), focused, &dates, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&tab.scroll)
                        .pt_1()
                        .pb(px(28.))
                        .size_full(),
                    )
                    .child(ui::scrollbar(
                        "scrollbar",
                        &tab.scroll,
                        self.scroll_drag.clone(),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        row: usize,
        cols: Rc<Vec<Col>>,
        focused: bool,
        dates: &DateFormatter,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tab) = self.tab() else {
            return div().into_any_element();
        };
        let entries = tab.browser.entries();
        let entry = tab.browser.order().get(row);
        let name = entries.name(entry);
        let kind = entries.kind(entry);
        let mode = entries.mode(entry);
        let selected = tab.browser.is_selected(row);
        let is_cursor = tab.browser.cursor == row && focused;
        let (category, label) = describe(kind, name, mode);
        let hue = category.hue();
        let highlights = if tab.filter.is_empty() {
            Vec::new()
        } else {
            match_ranges(name, &tab.filter)
        };
        let is_dir = kind.is_dir();
        // One allocation for the name, shared by the painted row and the
        // drag payload.
        let shared_name = SharedString::from(name.to_owned());
        let paint = RowPaint {
            icon: icon(category.icon()),
            icon_color: theme::category(hue),
            link: kind == Kind::Symlink,
            name: shared_name.clone(),
            name_color: if is_dir {
                theme::text()
            } else {
                theme::text_soft()
            },
            highlights,
            size: if is_dir {
                SharedString::new_static("—")
            } else {
                format::bytes(entries.size(entry)).into()
            },
            date: dates.short(entries.modified(entry)).into(),
            kind: SharedString::new_static(label),
            perms: if mode == 0 {
                SharedString::default()
            } else {
                format::permissions(mode).into()
            },
            cols,
            faded: ferry::model::settings::is_hidden(name),
        };
        let tab_id = tab.id;
        let mut el = div()
            .id(("row", row))
            .h(ROW_H)
            .mx_1()
            .rounded_md()
            .when(row % 2 == 1, |d| d.bg(theme::hover().opacity(0.4)))
            .when(selected, |d| {
                d.bg(if focused {
                    theme::accent_soft()
                } else {
                    theme::hover().opacity(1.8)
                })
            })
            .when(is_cursor && !selected, |d| {
                d.border_1().border_color(theme::accent().opacity(0.5))
            })
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.row_mouse_down(row, e, w, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.row_right_down(row, e, w, cx)
                }),
            )
            .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| this.row_click(row, e, w, cx)));

        // Dragging rows moves them onto a folder. The paths are resolved at
        // drop time, so a 100k selection costs nothing per frame.
        let with_selection = selected && tab.browser.selection().len() > 1;
        let count = if with_selection {
            tab.browser.selection().len()
        } else {
            1
        };
        let drag_label: SharedString = if count > 1 {
            format!("{} items", format::count(count as u64)).into()
        } else {
            shared_name.clone()
        };
        let drag_icon = if count > 1 { "layers" } else { category.icon() };
        el = el.on_drag(
            DraggedRows {
                tab: tab_id,
                name: shared_name,
                selection: with_selection,
            },
            move |_, _, _, cx| {
                cx.new(|_| DragPreview {
                    label: drag_label.clone(),
                    icon: drag_icon,
                    color: theme::category(hue),
                })
            },
        );
        if is_dir {
            // Only folders are drop targets, so only they need a full path.
            let into = path::join(&tab.browser.path, name);
            let into_ext = into.clone();
            el = el
                .drag_over::<DraggedRows>(|s, _, _, _| {
                    s.bg(theme::accent_strong())
                        .border_1()
                        .border_color(theme::accent())
                })
                .drag_over::<ExternalPaths>(|s, _, _, _| {
                    s.bg(theme::accent_strong())
                        .border_1()
                        .border_color(theme::accent())
                })
                .on_drop(cx.listener(move |this, d: &DraggedRows, _, cx| {
                    if let Some(paths) = this.dragged_paths(d) {
                        this.move_into(paths, into.clone(), cx);
                    }
                    cx.stop_propagation();
                }))
                .on_drop(cx.listener(move |this, d: &ExternalPaths, _, cx| {
                    this.upload_paths(d.paths().to_vec(), Some(into_ext.clone()), cx);
                    cx.stop_propagation();
                }));
        }
        el.child(paint).into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab() else {
            return div().into_any_element();
        };
        let ix = self.active;
        let mut actions: Vec<AnyElement> = Vec::new();
        let (icon_name, accent, title, detail, spinner): (
            &str,
            Hsla,
            SharedString,
            Option<SharedString>,
            bool,
        ) = match &tab.phase {
            Phase::Unlocking { .. } => (
                "key",
                theme::accent(),
                "Unlocking the keychain…".into(),
                Some(tab.connection.summary().into()),
                true,
            ),
            Phase::Connecting => (
                "plug",
                theme::accent(),
                format!("Connecting to {}…", tab.title()).into(),
                Some(tab.connection.summary().into()),
                true,
            ),
            Phase::Failed(why) => {
                actions.push(
                    ui::button("retry", "Try again", Some("refresh"), true)
                        .on_click(cx.listener(move |this, _, w, cx| this.reconnect(ix, w, cx)))
                        .into_any_element(),
                );
                if ferry::secrets::takes_secret(&tab.connection.auth) {
                    let tab_id = tab.id;
                    let prompt = match tab.connection.auth {
                        ferry::model::connection::Auth::AwsKeys { .. } => "Secret access key",
                        ferry::model::connection::Auth::KeyFile { .. } => "Key passphrase",
                        _ => "Password",
                    };
                    actions.push(
                        ui::button("enter-secret", format!("Enter {}", prompt.to_lowercase()), Some("key"), false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(ix) = this.tab_ix_by_id(tab_id) {
                                    this.tabs[ix].phase = Phase::Unlocking { asked: false };
                                }
                                this.ask_secret(tab_id, prompt, cx)
                            }))
                            .into_any_element(),
                    );
                }
                let id = tab.connection.id;
                actions.push(
                    ui::button("edit-conn", "Edit connection", Some("pencil"), false)
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.edit_connection(Some(id), ferry::model::connection::Protocol::Sftp, w, cx)
                        }))
                        .into_any_element(),
                );
                (
                    "unplug",
                    theme::danger(),
                    format!("Couldn't connect to {}", tab.title()).into(),
                    Some(if why.is_empty() {
                        "The connection was closed.".into()
                    } else {
                        why.clone()
                    }),
                    false,
                )
            }
            Phase::Ready => {
                if let Some(err) = &tab.browser.error {
                    if tab.can_back() {
                        actions.push(
                            ui::button("err-back", "Go back", Some("arrow-left"), false)
                                .on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))
                                .into_any_element(),
                        );
                    }
                    actions.push(
                        ui::button("err-retry", "Try again", Some("refresh"), false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.refresh_tab(ix);
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                    actions.push(
                        ui::button("err-reconnect", "Reconnect", Some("plug"), false)
                            .on_click(cx.listener(move |this, _, w, cx| this.reconnect(ix, w, cx)))
                            .into_any_element(),
                    );
                    (
                        "lock",
                        theme::danger(),
                        "Can't open this folder".into(),
                        Some(err.clone().into()),
                        false,
                    )
                } else if tab.browser.loading {
                    (
                        "folder-open",
                        theme::accent(),
                        "Loading…".into(),
                        Some(tab.browser.path.clone().into()),
                        true,
                    )
                } else if !tab.filter.is_empty() {
                    (
                        "search",
                        theme::faint(),
                        format!("Nothing here matches “{}”", tab.filter.trim()).into(),
                        Some("Press Esc to clear the filter.".into()),
                        false,
                    )
                } else {
                    let hidden = tab.browser.total();
                    actions.push(
                        ui::button("empty-upload", "Upload files", Some("upload"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.pick_uploads(false, cx)))
                            .into_any_element(),
                    );
                    actions.push(
                        ui::button("empty-folder", "New folder", Some("folder-plus"), false)
                            .on_click(cx.listener(|this, _, w, cx| this.ask_new_folder(w, cx)))
                            .into_any_element(),
                    );
                    (
                        "folder-open",
                        theme::faint(),
                        "This folder is empty".into(),
                        Some(if hidden > 0 && !self.settings.show_hidden {
                            format!(
                                "{} hidden — press Ctrl+H to show. Or drop files here to upload.",
                                format::items(hidden as u64)
                            )
                            .into()
                        } else {
                            "Drop files here to upload them.".into()
                        }),
                        false,
                    )
                }
            }
        };
        div()
            .id("empty")
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(
                div()
                    .size(px(60.))
                    .rounded_2xl()
                    .bg(accent.opacity(0.12))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_el(icon_name, accent, 26.)),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_soft())
                    .text_center()
                    .child(title),
            )
            .children(detail.map(|d| {
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(theme::muted())
                    .child(d)
            }))
            .when(spinner, |d| d.child(ui::busy_dots("connecting-dots")))
            .when(!actions.is_empty(), |d| {
                d.child(div().pt_2().flex().gap_2().children(actions))
            })
            .into_any_element()
    }
}

/// Byte ranges of `needle` in `name`, ignoring ASCII case.
fn match_ranges(name: &str, needle: &str) -> Vec<Range<usize>> {
    let needle = needle.trim();
    if needle.is_empty() || needle.len() > name.len() {
        return Vec::new();
    }
    let hay = name.as_bytes();
    let pat = needle.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + pat.len() <= hay.len() {
        if hay[i..i + pat.len()].eq_ignore_ascii_case(pat)
            && name.is_char_boundary(i)
            && name.is_char_boundary(i + pat.len())
        {
            out.push(i..i + pat.len());
            i += pat.len();
        } else {
            i += 1;
        }
    }
    out
}

pub struct DragPreview {
    label: SharedString,
    icon: &'static str,
    color: Hsla,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1p5()
            .rounded_lg()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::accent())
            .shadow_lg()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(theme::text())
            .child(icon_el(self.icon, self.color, 16.))
            .child(self.label.clone())
    }
}

// ── Custom-painted list row ─────────────────────────────────────────────

/// All the cells of a row painted directly: one layout node per row instead
/// of a dozen, which keeps scrolling a 100k-entry listing cheap.
struct RowPaint {
    icon: SharedString,
    icon_color: Hsla,
    link: bool,
    name: SharedString,
    name_color: Hsla,
    highlights: Vec<Range<usize>>,
    size: SharedString,
    date: SharedString,
    kind: SharedString,
    perms: SharedString,
    cols: Rc<Vec<Col>>,
    faded: bool,
}

impl IntoElement for RowPaint {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for RowPaint {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let painter = TextPainter::new(window, px(13.));
        let fade = |c: Hsla| if self.faded { c.opacity(0.55) } else { c };
        let spans = column_spans(bounds.size.width, &self.cols);
        let (top, h) = (bounds.top(), bounds.size.height);
        for (col, &(x, w)) in self.cols.iter().zip(&spans) {
            let x = bounds.left() + x;
            match col {
                Col::Name => {
                    let icon_b = Bounds::new(
                        point(x + px(12.), top + (h - px(16.)) / 2.),
                        size(px(16.), px(16.)),
                    );
                    let _ = window.paint_svg(
                        icon_b,
                        self.icon.clone(),
                        None,
                        Default::default(),
                        fade(self.icon_color),
                        cx,
                    );
                    if self.link {
                        let lb = Bounds::new(
                            point(icon_b.left() - px(3.), icon_b.bottom() - px(8.)),
                            size(px(10.), px(10.)),
                        );
                        window.paint_quad(gpui::fill(lb, theme::panel()).corner_radii(px(5.)));
                        let _ = window.paint_svg(
                            lb,
                            icon("link"),
                            None,
                            Default::default(),
                            theme::muted(),
                            cx,
                        );
                    }
                    let tx = x + px(38.);
                    painter.paint(
                        &self.name,
                        fade(self.name_color),
                        tx,
                        x + w - px(8.) - tx,
                        top,
                        h,
                        Align::Left,
                        &self.highlights,
                        window,
                        cx,
                    );
                }
                other => {
                    let (text, color) = match other {
                        Col::Size => (&self.size, theme::muted()),
                        Col::Modified => (&self.date, theme::muted()),
                        Col::Kind => (&self.kind, theme::faint()),
                        Col::Permissions => (&self.perms, theme::faint()),
                        Col::Name => unreachable!(),
                    };
                    painter.paint(
                        text,
                        fade(color),
                        x + px(8.),
                        w - px(16.),
                        top,
                        h,
                        other.align(),
                        &[],
                        window,
                        cx,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_case_insensitive_and_do_not_overlap() {
        assert_eq!(match_ranges("Report.PDF", "pdf"), vec![7..10]);
        assert_eq!(match_ranges("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(match_ranges("x", "").is_empty());
        assert!(match_ranges("x", "longer").is_empty());
    }

    #[test]
    fn spans_fill_the_row() {
        let cols = [Col::Name, Col::Size, Col::Modified];
        let spans = column_spans(px(800.), &cols);
        let (x, w) = spans[2];
        assert_eq!(x + w, px(800.));
    }

    #[test]
    fn rows_describe_themselves() {
        assert_eq!(describe(Kind::Directory, "x", 0).1, "Folder");
        assert_eq!(describe(Kind::Symlink, "x", 0).1, "Link");
        assert_eq!(describe(Kind::File, "a.png", 0o644).0, Category::Image);
    }
}
