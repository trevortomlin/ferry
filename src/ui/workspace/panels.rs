//! The parts of the window around the file list, each its own cached view.
//!
//! Scrolling, selecting and typing in the filter re-render the workspace.
//! Without this, every one of those frames also rebuilt the sidebar, tab
//! strip, toolbar, status bar and transfers drawer. Now each panel is reused
//! from the previous frame unless a signature of what it shows changed (see
//! `Workspace::sync_panels`), or something poked it directly — transfer
//! progress refreshes just the drawer and the status bar.

use std::hash::{DefaultHasher, Hash, Hasher};

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, StyleRefinement, WeakEntity, Window, div,
    prelude::*, px,
};

use super::{Phase, Workspace};
use crate::ui::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Sidebar,
    Tabs,
    Toolbar,
    Status,
    Transfers,
}

pub struct Panel {
    workspace: WeakEntity<Workspace>,
    kind: PanelKind,
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        // A cached view lays its content out as a new root: this is what
        // makes that content fill the panel rather than shrink to fit.
        let content = self.workspace
            .update(cx, |ws, cx| -> AnyElement {
                match kind {
                    PanelKind::Sidebar => ws.render_sidebar(cx).into_any_element(),
                    PanelKind::Tabs => ws.render_tabs(cx).into_any_element(),
                    PanelKind::Toolbar => ws.render_toolbar(window, cx).into_any_element(),
                    PanelKind::Status => ws.render_status(cx).into_any_element(),
                    PanelKind::Transfers => ws
                        .render_transfers(cx)
                        .unwrap_or_else(|| div().into_any_element()),
                }
            })
            .unwrap_or_else(|_| div().into_any_element());
        div().size_full().flex().flex_col().child(content)
    }
}

/// The panels, and the signature each was last drawn with.
pub struct Panels {
    pub sidebar: Entity<Panel>,
    pub tabs: Entity<Panel>,
    pub toolbar: Entity<Panel>,
    pub status: Entity<Panel>,
    pub transfers: Entity<Panel>,
    drawn: [u64; 5],
}

impl Panels {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        let workspace = cx.weak_entity();
        let mut make = |kind| {
            let workspace = workspace.clone();
            cx.new(|_| Panel { workspace, kind })
        };
        Self {
            sidebar: make(PanelKind::Sidebar),
            tabs: make(PanelKind::Tabs),
            toolbar: make(PanelKind::Toolbar),
            status: make(PanelKind::Status),
            transfers: make(PanelKind::Transfers),
            drawn: [0; 5],
        }
    }

    fn all(&self) -> [&Entity<Panel>; 5] {
        [
            &self.sidebar,
            &self.tabs,
            &self.toolbar,
            &self.status,
            &self.transfers,
        ]
    }

    /// Redraw the panels whose signature changed since they were drawn.
    pub fn sync(&mut self, now: [u64; 5], cx: &mut Context<Workspace>) {
        for (i, sig) in now.into_iter().enumerate() {
            if self.drawn[i] != sig {
                self.drawn[i] = sig;
                self.all()[i].update(cx, |_, cx| cx.notify());
            }
        }
    }

    /// Redraw these regardless of signatures (transfer progress, a listing
    /// still streaming in).
    pub fn poke(&self, which: &[PanelKind], cx: &mut Context<Workspace>) {
        for kind in which {
            let panel = match kind {
                PanelKind::Sidebar => &self.sidebar,
                PanelKind::Tabs => &self.tabs,
                PanelKind::Toolbar => &self.toolbar,
                PanelKind::Status => &self.status,
                PanelKind::Transfers => &self.transfers,
            };
            panel.update(cx, |_, cx| cx.notify());
        }
    }
}

pub fn sized(w: Option<f32>, h: Option<f32>) -> StyleRefinement {
    let mut s = StyleRefinement::default().flex_none();
    s = match w {
        Some(w) => s.w(px(w)),
        None => s.w_full(),
    };
    match h {
        Some(h) => s.h(px(h)),
        None => s.h_full(),
    }
}

fn hash(f: impl FnOnce(&mut DefaultHasher)) -> u64 {
    let mut h = DefaultHasher::new();
    theme::is_dark().hash(&mut h);
    f(&mut h);
    h.finish()
}

fn phase_tag(p: &Phase) -> u8 {
    match p {
        Phase::Unlocking { .. } => 0,
        Phase::Connecting => 1,
        Phase::Ready => 2,
        Phase::Failed(_) => 3,
    }
}

impl Workspace {
    /// What each panel shows, boiled down to a number. Anything a panel
    /// draws must feed its signature, or it will go stale.
    pub(super) fn panel_signatures(&self, window: &Window, cx: &gpui::App) -> [u64; 5] {
        use gpui::Focusable as _;
        let tab = self.tab();
        let sidebar = hash(|h| {
            for c in self.state.connections() {
                (c.id, &c.name, c.summary(), c.protocol as u8).hash(h);
            }
            for t in &self.tabs {
                (t.connection.id, phase_tag(&t.phase)).hash(h);
            }
            tab.map(|t| t.connection.id).hash(h);
            (self.transfers.active(), self.settings.show_transfers).hash(h);
        });
        let tabs = hash(|h| {
            for t in &self.tabs {
                (t.title(), phase_tag(&t.phase)).hash(h);
            }
            self.active.hash(h);
        });
        let toolbar = hash(|h| {
            if let Some(t) = tab {
                (
                    t.id,
                    phase_tag(&t.phase),
                    t.can_back(),
                    t.can_forward(),
                    t.can_up(),
                    &t.browser.path,
                    t.browser.loading,
                    t.browser.selection().is_empty(),
                    &t.connection.host,
                    &t.connection.bucket,
                )
                    .hash(h);
            }
            (self.settings.show_hidden, self.editing_location).hash(h);
            self.filter_input.read(cx).text().hash(h);
            self.filter_input.focus_handle(cx).is_focused(window).hash(h);
            self.location_input.focus_handle(cx).is_focused(window).hash(h);
        });
        let status = hash(|h| {
            if let Some(t) = tab {
                let b = &t.browser;
                (
                    t.id,
                    phase_tag(&t.phase),
                    b.loading,
                    b.visible(),
                    b.total(),
                    b.selection().len(),
                    t.filter.is_empty(),
                    b.load_time,
                    t.connection.summary(),
                )
                    .hash(h);
            }
            (self.transfers.overall(), self.transfers.active(), self.transfers.failed()).hash(h);
        });
        let transfers = hash(|h| {
            for t in self.transfers.all() {
                (t.id, t.done, t.total, &t.state, &t.name).hash(h);
            }
            self.open_after.len().hash(h);
        });
        [sidebar, tabs, toolbar, status, transfers]
    }
}
