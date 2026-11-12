//! Small presentational building blocks shared across views.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyView, App, Bounds, Context, ElementId, Font, FontWeight, Hsla, IntoElement, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, Stateful, TextAlign, TextRun,
    TruncateFrom, UniformListScrollHandle, Window, canvas, div, point, prelude::*, px, svg,
};

use crate::ui::assets::icon;
use crate::ui::theme;

pub struct TooltipText(pub SharedString);

impl Render for TooltipText {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border_strong())
            .shadow_md()
            .text_xs()
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

pub fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| TooltipText(text.into())).into()
}

pub fn icon_el(name: &str, color: Hsla, size_px: f32) -> gpui::Svg {
    svg()
        .path(icon(name))
        .size(px(size_px))
        .flex_none()
        .text_color(color)
}

pub fn icon_button(id: impl Into<ElementId>, name: &str, enabled: bool) -> Stateful<gpui::Div> {
    icon_toggle(id, name, enabled, false)
}

/// A square icon button; `on` draws it pressed (for view toggles).
pub fn icon_toggle(
    id: impl Into<ElementId>,
    name: &str,
    enabled: bool,
    on: bool,
) -> Stateful<gpui::Div> {
    let b = div()
        .id(id)
        .size(px(30.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_md()
        .when(on, |d| d.bg(theme::accent_soft()))
        .child(icon_el(
            name,
            if on {
                theme::accent()
            } else if enabled {
                theme::text_soft()
            } else {
                theme::faint()
            },
            16.0,
        ));
    if enabled {
        b.cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .active(|s| s.bg(theme::raised()))
    } else {
        b
    }
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_name: Option<&str>,
    primary: bool,
) -> Stateful<gpui::Div> {
    let mut b = div()
        .id(id)
        .h(px(30.0))
        .px_3()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .border_1();
    if primary {
        b = b
            .bg(theme::accent())
            .border_color(theme::accent())
            .text_color(theme::on_accent())
            .font_weight(FontWeight::SEMIBOLD)
            .hover(|s| s.opacity(0.9));
    } else {
        b = b
            .bg(theme::raised())
            .border_color(theme::border())
            .text_color(theme::text())
            .hover(|s| s.bg(theme::border()).border_color(theme::border_strong()));
    }
    if let Some(i) = icon_name {
        b = b.child(icon_el(
            i,
            if primary {
                theme::on_accent()
            } else {
                theme::muted()
            },
            14.0,
        ));
    }
    b.child(label.into())
}

pub fn usage_bar(fraction: f32, color: Hsla, height: f32) -> impl IntoElement {
    div()
        .w_full()
        .h(px(height))
        .rounded_full()
        .bg(theme::border())
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(color)
                .w(gpui::relative(fraction.clamp(0.0, 1.0))),
        )
}

pub fn kbd(text: &'static str) -> impl IntoElement {
    div()
        .px_1()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::border_strong())
        .text_xs()
        .text_color(theme::faint())
        .child(text)
}

// ── Text painting for custom elements ───────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Right,
}

/// Paints single lines of text straight into the scene; shaped lines are
/// cached by GPUI between frames.
#[derive(Clone)]
pub struct TextPainter {
    pub font: Font,
    pub font_size: Pixels,
    pub line_height: Pixels,
}

impl TextPainter {
    pub fn new(window: &Window, font_size: Pixels) -> Self {
        let style = window.text_style();
        TextPainter {
            font: style.font(),
            font_size,
            line_height: window.line_height(),
        }
    }

    /// Paints `text` in `[x, x + width)`, vertically centered in `[top, top +
    /// height)`, with an ellipsis when it doesn't fit. `highlights` are byte
    /// ranges drawn with a background (search matches).
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        text: &SharedString,
        color: Hsla,
        x: Pixels,
        width: Pixels,
        top: Pixels,
        height: Pixels,
        align: Align,
        highlights: &[Range<usize>],
        window: &mut Window,
        cx: &mut App,
    ) -> Pixels {
        if text.is_empty() || width <= px(4.) {
            return px(0.);
        }
        let base = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if highlights.is_empty() {
            vec![base]
        } else {
            let mut runs = Vec::new();
            let mut at = 0;
            for r in highlights {
                if r.start < at
                    || r.end > text.len()
                    || !text.is_char_boundary(r.start)
                    || !text.is_char_boundary(r.end)
                {
                    continue;
                }
                if r.start > at {
                    runs.push(TextRun {
                        len: r.start - at,
                        ..base.clone()
                    });
                }
                runs.push(TextRun {
                    len: r.end - r.start,
                    background_color: Some(theme::match_bg()),
                    color: theme::text(),
                    ..base.clone()
                });
                at = r.end;
            }
            if at < text.len() {
                runs.push(TextRun {
                    len: text.len() - at,
                    ..base.clone()
                });
            }
            runs
        };
        let mut line = window
            .text_system()
            .shape_line(text.clone(), self.font_size, &runs, None);
        if line.width() > width {
            let mut wrapper = cx
                .text_system()
                .line_wrapper(self.font.clone(), self.font_size);
            let (truncated, truncated_runs) =
                wrapper.truncate_line(text.clone(), width, "…", &runs, TruncateFrom::End);
            line =
                window
                    .text_system()
                    .shape_line(truncated, self.font_size, &truncated_runs, None);
        }
        let w = line.width();
        let x = match align {
            Align::Left => x,
            Align::Right => x + width - w,
        };
        let y = top + (height - self.line_height) / 2.;
        if !highlights.is_empty() {
            line.paint_background(
                point(x, y),
                self.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
        }
        line.paint(
            point(x, y),
            self.line_height,
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
        w
    }
}

// ── Scrollbar for uniform lists ─────────────────────────────────────────

const SCROLLBAR_WIDTH: Pixels = px(6.);
const SCROLLBAR_MIN_THUMB: Pixels = px(28.);

/// A draggable thumb over a `uniform_list`. `drag` holds where in the thumb a
/// drag started; `entity` is notified as it moves.
pub fn scrollbar<V: 'static>(
    id: impl Into<ElementId>,
    handle: &UniformListScrollHandle,
    drag: Rc<Cell<Option<Pixels>>>,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let (viewport, offset_y, content_height) = {
        let inner = handle.0.borrow();
        let content = inner
            .last_item_size
            .map(|s| s.contents.height)
            .unwrap_or_default();
        (
            inner.base_handle.bounds(),
            inner.base_handle.offset().y,
            content,
        )
    };
    let track = viewport.size.height;
    if track <= px(0.) || content_height <= track + px(1.) {
        return div().id(id);
    }
    let thumb = (track * (track / content_height)).max(SCROLLBAR_MIN_THUMB);
    let max_scroll = content_height - track;
    let progress = (-offset_y / max_scroll).clamp(0., 1.);
    let thumb_top = (track - thumb) * progress;
    let entity = cx.entity().downgrade();
    let scroll = handle.0.borrow().base_handle.clone();
    let dragging = drag.get().is_some();

    div()
        .id(id)
        .absolute()
        .top(thumb_top)
        .right(px(2.))
        .w(SCROLLBAR_WIDTH)
        .h(thumb)
        .rounded_full()
        .bg(if dragging {
            theme::muted()
        } else {
            theme::border_strong()
        })
        .hover(|s| s.bg(theme::muted()))
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds: Bounds<Pixels>, _, window, _| {
                    window.on_mouse_event({
                        let drag = drag.clone();
                        let entity = entity.clone();
                        move |ev: &MouseDownEvent, phase, _, cx| {
                            if phase.bubble() && thumb_bounds.contains(&ev.position) {
                                drag.set(Some(ev.position.y - thumb_bounds.origin.y));
                                if let Some(e) = entity.upgrade() {
                                    cx.notify(e.entity_id());
                                }
                                cx.stop_propagation();
                            }
                        }
                    });
                    window.on_mouse_event({
                        let drag = drag.clone();
                        let entity = entity.clone();
                        move |_: &MouseUpEvent, _, _, cx| {
                            if drag.take().is_some()
                                && let Some(e) = entity.upgrade()
                            {
                                cx.notify(e.entity_id());
                            }
                        }
                    });
                    window.on_mouse_event(move |ev: &MouseMoveEvent, _, _, cx| {
                        if !ev.dragging() {
                            return;
                        }
                        let Some(grab) = drag.get() else {
                            return;
                        };
                        let top = ev.position.y - viewport.origin.y - grab;
                        let progress = (top / (track - thumb)).clamp(0., 1.);
                        scroll.set_offset(point(px(0.), -(max_scroll * progress)));
                        if let Some(e) = entity.upgrade() {
                            cx.notify(e.entity_id());
                        }
                    });
                },
            )
            .size_full(),
        )
}

// ── Small animated and form widgets ─────────────────────────────────────

/// Three dots breathing in turn: "working on it". Capped at 30 fps so a
/// long wait costs next to nothing.
pub fn busy_dots(id: &'static str) -> impl IntoElement {
    dots(id, 7., 6.)
}

/// A smaller version for the status bar.
pub fn busy_dots_small(id: &'static str) -> impl IntoElement {
    dots(id, 4., 3.)
}

fn dots(id: &'static str, size_px: f32, gap: f32) -> impl IntoElement {
    use gpui::{Animation, AnimationExt};
    div().flex().items_center().gap(px(gap)).children((0..3usize).map(move |i| {
        div()
            .size(px(size_px))
            .rounded_full()
            .bg(theme::accent())
            .with_animation(
                (id, i),
                Animation::new(std::time::Duration::from_millis(1100))
                    .repeat_synced()
                    .with_max_fps(30.),
                move |d, t| {
                    let phase = (t - i as f32 * 0.18).rem_euclid(1.0);
                    let lift = 1.0 - (phase * 2.0 - 1.0).abs();
                    d.opacity(0.25 + 0.75 * lift)
                },
            )
    }))
}

/// One option of a segmented control.
pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_name: Option<&str>,
    on: bool,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1p5()
        .rounded(px(6.))
        .text_sm()
        .cursor_pointer()
        .when(on, |d| {
            d.bg(theme::raised())
                .text_color(theme::text())
                .font_weight(FontWeight::MEDIUM)
                .shadow_sm()
                .border_1()
                .border_color(theme::border_strong())
        })
        .when(!on, |d| {
            d.text_color(theme::muted())
                .hover(|s| s.text_color(theme::text()))
        })
        .when_some(icon_name, |d, i| {
            d.child(icon_el(
                i,
                if on { theme::accent() } else { theme::faint() },
                14.,
            ))
        })
        .child(label.into())
}

/// The track a row of `segment`s sits in.
pub fn segmented() -> gpui::Div {
    div()
        .flex()
        .p(px(3.))
        .gap(px(2.))
        .rounded_lg()
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
}

/// An on/off switch.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(34.))
        .h(px(20.))
        .flex_none()
        .rounded_full()
        .p(px(2.))
        .flex()
        .cursor_pointer()
        .when(on, |d| d.bg(theme::accent()).justify_end())
        .when(!on, |d| d.bg(theme::border_strong()))
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .bg(if on { theme::on_accent() } else { theme::panel() })
                .shadow_sm(),
        )
}
