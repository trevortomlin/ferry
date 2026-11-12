// The state of one directory being looked at.
//
// The interesting rule lives in `apply`: every request carries a generation and
// every reply echoes it, so a reply for a directory the user has already
// navigated away from is dropped rather than drawn. Without that, a slow server
// makes the view flicker between two directories, and on a high-latency link
// that is the normal case rather than an edge case.
//
// This is plain data with no I/O, so all of it is testable directly.

use std::time::{Duration, Instant};

use crate::model::entries::Entries;
use crate::model::message::{Event, Generation, SessionId};
use crate::model::order::{Filter, Order, Sort};
use crate::model::path;
use crate::model::selection::Selection;

/// What moving the cursor should do to the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// Plain arrow key or click: the cursor row becomes the whole selection.
    Replace,
    /// Shift: select everything between the anchor and the cursor.
    Extend,
    /// Ctrl: flip just this row, leave the rest alone.
    Toggle,
}

#[derive(Debug, Default)]
pub struct Browser {
    pub session: Option<SessionId>,
    pub path: String,
    /// The request the visible entries belong to.
    generation: Generation,
    /// Bumped on every request; only replies matching it are accepted.
    latest: Generation,
    entries: Entries,
    order: Order,
    pub loading: bool,
    pub error: Option<String>,
    /// The row the keyboard is on, as an index into `order`.
    pub cursor: usize,
    /// Where a shift-range starts, as a row.
    anchor: usize,
    /// Selected entries, by entry index rather than row, so the selection
    /// survives sorting and filtering.
    selection: Selection,
    /// Set when the cursor moves by keyboard, so the view scrolls to it once.
    pub scroll_to_cursor: bool,
    /// When the visible order was last recomputed, so a streaming listing does
    /// not re-sort after every chunk.
    last_reorder: Option<Instant>,
    /// When the current request was issued, for the load-time readout.
    started: Option<Instant>,
    /// How long the last completed listing took.
    pub load_time: Option<Duration>,
}

/// A 100k-entry directory arrives in roughly two hundred chunks. Re-sorting
/// after each one is quadratic — measured at 3.4 seconds of pure sorting for a
/// listing that transfers in well under a second. Chunks also arrive far faster
/// than anyone can read them, so the order is refreshed at most this often
/// while loading, and always once when the last chunk lands.
const REORDER_INTERVAL: Duration = Duration::from_millis(100);

impl Browser {
    /// Start looking at `path`. Returns the generation to send with the
    /// request.
    pub fn navigate(&mut self, path: impl Into<String>) -> Generation {
        self.path = path.into();
        self.entries.clear();
        self.order
            .rebuild(&self.entries, Sort::default(), Filter::new("", true));
        self.error = None;
        self.loading = true;
        self.cursor = 0;
        self.anchor = 0;
        self.selection.clear();
        self.last_reorder = None;
        self.started = Some(Instant::now());
        self.load_time = None;
        self.generation = self.latest.bump();
        self.generation
    }

    /// Re-request the current directory.
    pub fn refresh(&mut self) -> Generation {
        let path = std::mem::take(&mut self.path);
        self.navigate(path)
    }

    /// Release memory the finished listing no longer needs.
    pub fn shrink(&mut self) {
        self.entries.shrink_to_fit();
        self.order.shrink_to_fit();
    }

    pub fn entries(&self) -> &Entries {
        &self.entries
    }

    pub fn order(&self) -> &Order {
        &self.order
    }

    /// Number of rows currently visible, after filtering.
    pub fn visible(&self) -> usize {
        self.order.len()
    }

    pub fn total(&self) -> usize {
        self.entries.len()
    }

    /// The absolute path of visible row `row`.
    pub fn path_of(&self, row: usize) -> Option<String> {
        (row < self.order.len()).then(|| {
            let index = self.order.get(row);
            path::join(&self.path, self.entries.name(index))
        })
    }

    pub fn is_dir(&self, row: usize) -> bool {
        row < self.order.len() && self.entries.kind(self.order.get(row)).is_dir()
    }

    /// Recompute the visible order. Called when the sort or the filter changes,
    /// not every frame.
    ///
    /// The cursor follows the entry it was on rather than staying at the same
    /// row number: re-sorting a directory while a file is highlighted should
    /// keep that file highlighted, not jump to whatever now sits at that row.
    pub fn reorder(&mut self, sort: Sort, filter: Filter<'_>) {
        let was_on = self.cursor_entry();
        self.order.rebuild(&self.entries, sort, filter);

        self.cursor = was_on
            .and_then(|entry| self.order.iter().position(|i| i == entry))
            .unwrap_or_else(|| self.cursor.min(self.order.len().saturating_sub(1)));
        self.anchor = self.anchor.min(self.order.len().saturating_sub(1));
    }

    // ---- selection ----

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The entry the cursor is on, if any row is.
    pub fn cursor_entry(&self) -> Option<usize> {
        (self.cursor < self.order.len()).then(|| self.order.get(self.cursor))
    }

    pub fn is_selected(&self, row: usize) -> bool {
        row < self.order.len() && self.selection.contains(self.order.get(row))
    }

    /// Put the cursor on `row` and update the selection accordingly.
    pub fn focus(&mut self, row: usize, how: Move) {
        if self.order.is_empty() {
            return;
        }
        let row = row.min(self.order.len() - 1);
        self.cursor = row;

        match how {
            Move::Replace => {
                self.selection.clear();
                self.selection.set(self.order.get(row), true);
                self.anchor = row;
            }
            Move::Toggle => {
                self.selection.toggle(self.order.get(row));
                self.anchor = row;
            }
            Move::Extend => {
                // The anchor stays put; everything between it and the cursor
                // becomes the selection.
                self.selection.clear();
                let (from, to) = if self.anchor <= row {
                    (self.anchor, row)
                } else {
                    (row, self.anchor)
                };
                for r in from..=to {
                    self.selection.set(self.order.get(r), true);
                }
            }
        }
    }

    /// Move the cursor by `delta` rows, clamped to the list.
    pub fn move_cursor(&mut self, delta: isize, how: Move) {
        if self.order.is_empty() {
            return;
        }
        let last = self.order.len() as isize - 1;
        let target = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.focus(target, how);
        self.scroll_to_cursor = true;
    }

    pub fn select_all(&mut self) {
        self.selection.clear();
        for entry in self.order.iter() {
            self.selection.set(entry, true);
        }
        self.anchor = 0;
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Absolute paths of everything selected, in visible order.
    pub fn selected_paths(&self) -> Vec<String> {
        self.order
            .iter()
            .filter(|&entry| self.selection.contains(entry))
            .map(|entry| path::join(&self.path, self.entries.name(entry)))
            .collect()
    }

    /// Selected rows paired with whether each is a directory, in visible order.
    pub fn selected_rows(&self) -> Vec<(String, bool)> {
        self.order
            .iter()
            .filter(|&entry| self.selection.contains(entry))
            .map(|entry| {
                (
                    path::join(&self.path, self.entries.name(entry)),
                    self.entries.kind(entry).is_dir(),
                )
            })
            .collect()
    }

    /// The next row at or after `from` whose name starts with `prefix`, for
    /// type-ahead. Wraps around.
    pub fn find_prefix(&self, from: usize, prefix: &str) -> Option<usize> {
        if prefix.is_empty() || self.order.is_empty() {
            return None;
        }
        let rows = self.order.len();
        (0..rows).map(|offset| (from + offset) % rows).find(|&row| {
            let name = self.entries.name(self.order.get(row));
            name.len() >= prefix.len()
                && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        })
    }

    /// Fold in one engine event. Returns true if anything changed on screen,
    /// which is also the answer to "does this need a repaint?".
    pub fn apply(&mut self, event: &mut Event, sort: Sort, filter: Filter<'_>) -> bool {
        match event {
            Event::Listing {
                session,
                generation,
                entries,
                last,
            } => {
                if !self.accepts(*session, *generation) {
                    return false;
                }
                self.entries.append(entries);
                self.loading = !*last;

                let due = self
                    .last_reorder
                    .is_none_or(|at| at.elapsed() >= REORDER_INTERVAL);
                if *last || due {
                    self.reorder(sort, filter);
                    self.last_reorder = Some(Instant::now());
                }
                if *last && let Some(started) = self.started.take() {
                    let elapsed = started.elapsed();
                    self.load_time = Some(elapsed);
                    tracing::debug!(
                        entries = self.entries.len(),
                        millis = elapsed.as_secs_f32() * 1000.0,
                        kib = self.entries.heap_bytes() / 1024,
                        "listing complete"
                    );
                }
                true
            }
            Event::Failed {
                session,
                generation,
                message,
            } => {
                if !self.accepts(*session, *generation) {
                    return false;
                }
                self.loading = false;
                self.error = Some(std::mem::take(message));
                true
            }
            Event::Cancelled {
                session,
                generation,
            } => {
                if !self.accepts(*session, *generation) {
                    return false;
                }
                self.loading = false;
                true
            }
            // Transfers and file operations are the app's business, not one
            // directory's.
            _ => false,
        }
    }

    /// A reply is ours only if it is for this session *and* for the request we
    /// are still waiting on.
    fn accepts(&self, session: SessionId, generation: Generation) -> bool {
        self.session == Some(session) && generation == self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entries::{Entry, Kind};

    const SESSION: SessionId = SessionId(1);

    fn chunk(names: &[(&str, Kind)]) -> Entries {
        let mut entries = Entries::new();
        for (name, kind) in names {
            entries.push(Entry {
                name,
                size: 0,
                modified: 0,
                mode: 0,
                kind: *kind,
            });
        }
        entries
    }

    fn listing(generation: Generation, names: &[(&str, Kind)], last: bool) -> Event {
        Event::Listing {
            session: SESSION,
            generation,
            entries: chunk(names),
            last,
        }
    }

    fn opened() -> Browser {
        Browser {
            session: Some(SESSION),
            ..Default::default()
        }
    }

    #[test]
    fn chunks_accumulate_and_the_last_one_ends_the_load() {
        let mut browser = opened();
        let generation = browser.navigate("/home/ferry");
        assert!(browser.loading);

        let mut first = listing(generation, &[("a.txt", Kind::File)], false);
        assert!(browser.apply(&mut first, Sort::default(), Filter::new("", true)));
        assert!(browser.loading, "more is still coming");
        assert_eq!(browser.visible(), 1);

        let mut second = listing(generation, &[("b.txt", Kind::File)], true);
        assert!(browser.apply(&mut second, Sort::default(), Filter::new("", true)));
        assert!(!browser.loading);
        assert_eq!(browser.visible(), 2);
    }

    #[test]
    fn a_reply_for_the_previous_directory_is_dropped() {
        let mut browser = opened();
        let stale = browser.navigate("/slow");
        let current = browser.navigate("/fast");
        assert_ne!(stale, current);

        let mut late = listing(stale, &[("ghost.txt", Kind::File)], true);
        assert!(
            !browser.apply(&mut late, Sort::default(), Filter::new("", true)),
            "the old directory's reply must not be drawn"
        );
        assert_eq!(browser.visible(), 0);
        assert!(browser.loading, "still waiting on /fast");

        let mut wanted = listing(current, &[("real.txt", Kind::File)], true);
        assert!(browser.apply(&mut wanted, Sort::default(), Filter::new("", true)));
        assert_eq!(browser.visible(), 1);
    }

    #[test]
    fn a_reply_for_another_session_is_dropped() {
        let mut browser = opened();
        let generation = browser.navigate("/home");
        let mut foreign = Event::Listing {
            session: SessionId(99),
            generation,
            entries: chunk(&[("x", Kind::File)]),
            last: true,
        };
        assert!(!browser.apply(&mut foreign, Sort::default(), Filter::new("", true)));
        assert_eq!(browser.visible(), 0);
    }

    #[test]
    fn a_stale_failure_does_not_clear_a_fresh_load() {
        let mut browser = opened();
        let stale = browser.navigate("/slow");
        browser.navigate("/fast");

        let mut failure = Event::Failed {
            session: SESSION,
            generation: stale,
            message: "connection reset".to_owned(),
        };
        assert!(!browser.apply(&mut failure, Sort::default(), Filter::new("", true)));
        assert!(browser.error.is_none());
        assert!(browser.loading);
    }

    #[test]
    fn a_current_failure_stops_the_load_and_is_shown() {
        let mut browser = opened();
        let generation = browser.navigate("/nope");
        let mut failure = Event::Failed {
            session: SESSION,
            generation,
            message: "Permission denied".to_owned(),
        };
        assert!(browser.apply(&mut failure, Sort::default(), Filter::new("", true)));
        assert!(!browser.loading);
        assert_eq!(browser.error.as_deref(), Some("Permission denied"));
    }

    #[test]
    fn navigating_clears_the_previous_listing_immediately() {
        let mut browser = opened();
        let generation = browser.navigate("/a");
        let mut arrived = listing(generation, &[("one", Kind::File)], true);
        browser.apply(&mut arrived, Sort::default(), Filter::new("", true));
        assert_eq!(browser.visible(), 1);

        browser.navigate("/b");
        assert_eq!(browser.visible(), 0, "no stale rows while /b loads");
        assert!(browser.error.is_none());
    }

    #[test]
    fn a_streaming_listing_does_not_reorder_after_every_chunk() {
        let mut browser = opened();
        let generation = browser.navigate("/big");

        // The first chunk always shows, so the list is not blank while loading.
        let mut first = listing(generation, &[("b", Kind::File)], false);
        browser.apply(&mut first, Sort::default(), Filter::new("", true));
        assert_eq!(browser.visible(), 1);

        // Chunks arriving inside the interval are accumulated but not sorted
        // again — `total` grows while `visible` waits.
        for name in ["c", "d", "e"] {
            let mut chunk = listing(generation, &[(name, Kind::File)], false);
            browser.apply(&mut chunk, Sort::default(), Filter::new("", true));
        }
        assert_eq!(browser.total(), 4);
        assert_eq!(
            browser.visible(),
            1,
            "mid-stream chunks should not each trigger a sort"
        );

        // The final chunk always reorders, so the finished list is complete and
        // correctly sorted.
        let mut last = listing(generation, &[("a", Kind::File)], true);
        browser.apply(&mut last, Sort::default(), Filter::new("", true));
        assert!(!browser.loading);
        assert_eq!(browser.visible(), 5);
        let entries = browser.entries();
        let names: Vec<_> = browser
            .order()
            .iter()
            .map(|i| entries.name(i).to_owned())
            .collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn refresh_re_requests_the_same_path_under_a_new_generation() {
        let mut browser = opened();
        let first = browser.navigate("/a");
        let second = browser.refresh();
        assert_eq!(browser.path, "/a");
        assert_ne!(first, second);
    }

    #[test]
    fn rows_resolve_to_absolute_paths_through_the_sort_order() {
        let mut browser = opened();
        let generation = browser.navigate("/home/ferry");
        let mut arrived = listing(
            generation,
            &[
                ("zebra.txt", Kind::File),
                ("docs", Kind::Directory),
                ("apple.txt", Kind::File),
            ],
            true,
        );
        browser.apply(&mut arrived, Sort::default(), Filter::new("", true));

        // Directories first, then name order.
        assert_eq!(browser.path_of(0).as_deref(), Some("/home/ferry/docs"));
        assert!(browser.is_dir(0));
        assert_eq!(browser.path_of(1).as_deref(), Some("/home/ferry/apple.txt"));
        assert!(!browser.is_dir(1));
        assert_eq!(browser.path_of(3), None);
    }

    #[test]
    fn filtering_narrows_the_visible_rows_without_losing_the_listing() {
        let mut browser = opened();
        let generation = browser.navigate("/x");
        let mut arrived = listing(
            generation,
            &[
                ("alpha.txt", Kind::File),
                ("beta.log", Kind::File),
                ("gamma.txt", Kind::File),
            ],
            true,
        );
        browser.apply(&mut arrived, Sort::default(), Filter::new("", true));

        browser.reorder(Sort::default(), Filter::new(".txt", true));
        assert_eq!(browser.visible(), 2);
        assert_eq!(browser.total(), 3, "the listing itself is untouched");

        browser.reorder(Sort::default(), Filter::new("", true));
        assert_eq!(browser.visible(), 3);
    }

    /// Builds a loaded browser with the given names, sorted, cursor at 0.
    fn loaded(names: &[(&str, Kind)]) -> Browser {
        let mut browser = opened();
        let generation = browser.navigate("/x");
        let mut event = listing(generation, names, true);
        browser.apply(&mut event, Sort::default(), Filter::new("", true));
        browser
    }

    fn names_of(browser: &Browser, rows: &[usize]) -> Vec<String> {
        rows.iter()
            .map(|&row| browser.entries().name(browser.order().get(row)).to_owned())
            .collect()
    }

    #[test]
    fn arrow_keys_move_the_cursor_and_replace_the_selection() {
        let mut browser = loaded(&[("a", Kind::File), ("b", Kind::File), ("c", Kind::File)]);
        browser.focus(0, Move::Replace);
        assert_eq!(browser.selection().len(), 1);
        assert!(browser.is_selected(0));

        browser.move_cursor(1, Move::Replace);
        assert_eq!(browser.cursor, 1);
        assert!(browser.is_selected(1));
        assert!(!browser.is_selected(0), "a plain move replaces");
        assert!(browser.scroll_to_cursor);
    }

    #[test]
    fn the_cursor_stops_at_the_ends_rather_than_wrapping() {
        let mut browser = loaded(&[("a", Kind::File), ("b", Kind::File)]);
        browser.focus(0, Move::Replace);
        browser.move_cursor(-5, Move::Replace);
        assert_eq!(browser.cursor, 0);
        browser.move_cursor(99, Move::Replace);
        assert_eq!(browser.cursor, 1);
    }

    #[test]
    fn shift_extends_from_the_anchor_in_both_directions() {
        let mut browser = loaded(&[
            ("a", Kind::File),
            ("b", Kind::File),
            ("c", Kind::File),
            ("d", Kind::File),
        ]);
        browser.focus(1, Move::Replace);
        browser.focus(3, Move::Extend);
        assert_eq!(browser.selection().len(), 3);
        assert_eq!(names_of(&browser, &[1, 2, 3]), ["b", "c", "d"]);

        // Extending back past the anchor selects the other side, not both.
        browser.focus(0, Move::Extend);
        assert_eq!(browser.selection().len(), 2);
        assert!(browser.is_selected(0) && browser.is_selected(1));
        assert!(!browser.is_selected(2));
    }

    #[test]
    fn ctrl_toggles_one_row_and_leaves_the_rest() {
        let mut browser = loaded(&[("a", Kind::File), ("b", Kind::File), ("c", Kind::File)]);
        browser.focus(0, Move::Replace);
        browser.focus(2, Move::Toggle);
        assert_eq!(browser.selection().len(), 2);
        assert!(browser.is_selected(0) && browser.is_selected(2));

        browser.focus(2, Move::Toggle);
        assert_eq!(browser.selection().len(), 1);
        assert!(browser.is_selected(0));
    }

    #[test]
    fn a_selection_survives_re_sorting() {
        let mut browser = loaded(&[
            ("apple", Kind::File),
            ("banana", Kind::File),
            ("cherry", Kind::File),
        ]);
        browser.focus(0, Move::Replace);
        assert_eq!(browser.selected_paths(), ["/x/apple"]);

        browser.reorder(
            Sort {
                descending: true,
                ..Sort::default()
            },
            Filter::new("", true),
        );

        // Same file, different row.
        assert_eq!(browser.selected_paths(), ["/x/apple"]);
        assert_eq!(browser.cursor, 2, "the cursor follows the entry");
        assert!(browser.is_selected(2));
    }

    #[test]
    fn a_selection_survives_filtering_and_comes_back() {
        let mut browser = loaded(&[("notes.txt", Kind::File), ("photo.png", Kind::File)]);
        browser.focus(1, Move::Replace);
        assert_eq!(browser.selected_paths(), ["/x/photo.png"]);

        // Filtered out: not visible, so not reported.
        browser.reorder(Sort::default(), Filter::new("notes", true));
        assert!(browser.selected_paths().is_empty());

        // Back again once the filter clears.
        browser.reorder(Sort::default(), Filter::new("", true));
        assert_eq!(browser.selected_paths(), ["/x/photo.png"]);
    }

    #[test]
    fn select_all_covers_only_what_is_visible() {
        let mut browser = loaded(&[
            ("alpha.txt", Kind::File),
            ("beta.log", Kind::File),
            ("gamma.txt", Kind::File),
        ]);
        browser.reorder(Sort::default(), Filter::new(".txt", true));
        browser.select_all();
        assert_eq!(browser.selection().len(), 2);
        assert_eq!(browser.selected_paths(), ["/x/alpha.txt", "/x/gamma.txt"]);
    }

    #[test]
    fn selected_rows_reports_directories() {
        let mut browser = loaded(&[("docs", Kind::Directory), ("a.txt", Kind::File)]);
        browser.select_all();
        assert_eq!(
            browser.selected_rows(),
            [("/x/docs".to_owned(), true), ("/x/a.txt".to_owned(), false),]
        );
    }

    #[test]
    fn navigating_drops_the_selection() {
        let mut browser = loaded(&[("a", Kind::File)]);
        browser.select_all();
        assert_eq!(browser.selection().len(), 1);
        browser.navigate("/elsewhere");
        assert!(browser.selection().is_empty());
        assert_eq!(browser.cursor, 0);
    }

    #[test]
    fn type_ahead_finds_the_next_match_and_wraps() {
        let browser = loaded(&[
            ("alpha", Kind::File),
            ("beta", Kind::File),
            ("Bravo", Kind::File),
        ]);
        // Sorted: alpha, beta, Bravo.
        assert_eq!(browser.find_prefix(0, "b"), Some(1));
        assert_eq!(browser.find_prefix(2, "b"), Some(2));
        // Past the end, wraps to the first match.
        assert_eq!(browser.find_prefix(2, "a"), Some(0));
        assert_eq!(browser.find_prefix(0, "zz"), None);
        assert_eq!(browser.find_prefix(0, ""), None);
    }

    #[test]
    fn selection_operations_on_an_empty_listing_do_nothing() {
        let mut browser = opened();
        browser.navigate("/empty");
        browser.focus(0, Move::Replace);
        browser.move_cursor(1, Move::Replace);
        browser.select_all();
        assert!(browser.selection().is_empty());
        assert!(browser.selected_paths().is_empty());
        assert_eq!(browser.find_prefix(0, "a"), None);
    }

    #[test]
    fn the_cursor_never_points_past_the_last_visible_row() {
        let mut browser = opened();
        let generation = browser.navigate("/x");
        let mut arrived = listing(
            generation,
            &[("a", Kind::File), ("b", Kind::File), ("c", Kind::File)],
            true,
        );
        browser.apply(&mut arrived, Sort::default(), Filter::new("", true));

        browser.cursor = 2;
        browser.reorder(Sort::default(), Filter::new("a", true));
        assert_eq!(browser.visible(), 1);
        assert_eq!(browser.cursor, 0);
    }
}
