// Sorting and filtering, as a permutation.
//
// `Entries` is never reordered. Instead `Order` holds indices into it, and the
// file list draws row `n` by looking at `order.get(n)`. Re-sorting 100k entries
// then costs shuffling 400 KB of u32 rather than moving several megabytes of
// columns, and the arena stays valid for anything else holding an index.

use serde::{Deserialize, Serialize};

use crate::model::entries::Entries;
use crate::model::settings::is_hidden;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
}

impl SortKey {
    pub const ALL: [Self; 3] = [Self::Name, Self::Size, Self::Modified];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Modified => "Modified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
    /// Directories before files regardless of key, as every file manager does.
    pub directories_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
            directories_first: true,
        }
    }
}

impl Sort {
    /// Clicking the active column flips direction; clicking another switches to
    /// it, ascending.
    pub fn toggle(&mut self, key: SortKey) {
        if self.key == key {
            self.descending = !self.descending;
        } else {
            self.key = key;
            self.descending = false;
        }
    }
}

/// What the file list should show, as opposed to how it should be ordered.
#[derive(Debug, Clone, Copy)]
pub struct Filter<'a> {
    /// Case-insensitive substring of the name; empty matches everything.
    pub needle: &'a str,
    pub show_hidden: bool,
}

impl<'a> Filter<'a> {
    pub fn new(needle: &'a str, show_hidden: bool) -> Self {
        Self {
            needle,
            show_hidden,
        }
    }

    /// Nothing to do, so the sorted order can be used directly.
    fn shows_everything(&self) -> bool {
        self.needle.is_empty() && self.show_hidden
    }
}

#[derive(Debug, Default, Clone)]
pub struct Order {
    /// Every entry, in sort order.
    ///
    /// Kept separate from the filtered view because filtering preserves
    /// relative order: once this is sorted, narrowing it is a linear scan, not
    /// another sort. Typing in the filter box used to re-sort the whole
    /// directory on every keystroke.
    sorted: Vec<u32>,
    /// The subset of `sorted` passing the current filter. Unused, and not
    /// copied into, while the filter is empty.
    indices: Vec<u32>,
    /// Whether `indices` is in use, or `sorted` is the visible order.
    filtered: bool,
    /// The listing revision and sort `sorted` was computed for. Anything else
    /// means it has to be redone.
    sorted_for: Option<(u64, Sort)>,
    /// The first eight bytes of each name, lowercased and packed big-endian,
    /// indexed by entry.
    ///
    /// Sorting compares names roughly n log n times, and reading a name means
    /// two lookups in `ends` plus a slice into the name buffer — three
    /// dependent loads scattered across megabytes. That cache miss, not the
    /// comparison itself, was what made sorting 100k entries slow. Comparing a
    /// dense array of `u64` instead resolves almost every comparison with one
    /// load, and the full name is consulted only when eight bytes tie.
    ///
    /// Costs eight bytes per entry, rebuilt in one sequential pass.
    keys: Vec<u64>,
    /// Byte offset the keys start at: the length of the prefix every name in
    /// the listing shares.
    ///
    /// A directory of `file-000001.dat`, `file-000002.dat` … defeats a key
    /// taken from the front, because all eight bytes are identical and every
    /// comparison falls through to the full name anyway. Skipping the shared
    /// part first puts the eight discriminating bytes in the key instead.
    /// Widening the key to sixteen bytes fixes that case too, but costs twice
    /// the memory and measurably slows the ordinary case; this does neither.
    key_offset: usize,
}

impl Order {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rows on screen, in order.
    fn active(&self) -> &[u32] {
        if self.filtered {
            &self.indices
        } else {
            &self.sorted
        }
    }

    pub fn shrink_to_fit(&mut self) {
        self.sorted.shrink_to_fit();
        self.indices.shrink_to_fit();
    }

    pub fn len(&self) -> usize {
        self.active().len()
    }

    pub fn is_empty(&self) -> bool {
        self.active().is_empty()
    }

    /// Index into the backing `Entries` for visible row `row`.
    pub fn get(&self, row: usize) -> usize {
        self.active()[row] as usize
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.active().iter().map(|&i| i as usize)
    }

    /// Recompute the visible order.
    ///
    /// The sort is skipped when neither the listing nor the sort has changed
    /// since last time, which is what makes filtering cheap.
    pub fn rebuild(&mut self, entries: &Entries, sort: Sort, filter: Filter<'_>) {
        if self.sorted_for != Some((entries.revision(), sort)) {
            self.resort(entries, sort);
            self.sorted_for = Some((entries.revision(), sort));
        }
        self.refilter(entries, filter);
    }

    /// Narrow the sorted order to what should be visible. Linear, and it
    /// preserves the sort because it only ever removes.
    fn refilter(&mut self, entries: &Entries, filter: Filter<'_>) {
        if filter.shows_everything() {
            self.filtered = false;
            self.indices.clear();
            return;
        }
        self.filtered = true;
        self.indices.clear();
        self.indices.reserve(self.sorted.len());
        // Lowercased once, then compared byte-wise, so matching allocates
        // nothing per entry.
        let needle = filter.needle.to_lowercase();
        for &i in &self.sorted {
            let name = entries.name(i as usize);
            if !filter.show_hidden && is_hidden(name) {
                continue;
            }
            if !needle.is_empty() && !contains_ignore_ascii_case(name, &needle) {
                continue;
            }
            self.indices.push(i);
        }
    }

    fn resort(&mut self, entries: &Entries, sort: Sort) {
        self.sorted.clear();
        self.sorted.reserve(entries.len());
        self.sorted.extend(0..entries.len() as u32);

        let key = sort.key;
        let descending = sort.descending;
        let dirs_first = sort.directories_first;

        // One sequential pass, reused by both the primary name comparison and
        // the tie-break every other sort key falls back to.
        let mut keys = std::mem::take(&mut self.keys);
        keys.clear();
        keys.reserve(entries.len());
        self.key_offset = shared_prefix_len(entries);
        let offset = self.key_offset;
        for i in 0..entries.len() {
            keys.push(prefix_key(entries.name(i), offset));
        }

        self.sorted.sort_unstable_by(|&a, &b| {
            let (a, b) = (a as usize, b as usize);

            if dirs_first {
                let ordering = entries.kind(b).is_dir().cmp(&entries.kind(a).is_dir());
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }

            let ordering = match key {
                // The packed prefix decides it unless the first eight bytes
                // are identical, which is the only case that reads a name.
                SortKey::Name => keys[a]
                    .cmp(&keys[b])
                    .then_with(|| cmp_name(entries.name(a), entries.name(b))),
                SortKey::Size => entries.size(a).cmp(&entries.size(b)),
                SortKey::Modified => entries.modified(a).cmp(&entries.modified(b)),
            };
            let ordering = if descending {
                ordering.reverse()
            } else {
                ordering
            };

            // Name breaks ties so equal sizes or timestamps still render in a
            // stable, predictable order.
            ordering.then_with(|| {
                keys[a]
                    .cmp(&keys[b])
                    .then_with(|| cmp_name(entries.name(a), entries.name(b)))
            })
        });

        self.keys = keys;
    }
}

/// Case-insensitive name comparison without allocating. ASCII-only folding:
/// correct for the overwhelming majority of filenames and it avoids building a
/// lowercased copy of every name on every sort.
fn cmp_name(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let shared = a.len().min(b.len());
    // An indexed loop rather than zipped iterator adaptors: this runs roughly
    // n log n times per sort, and at 100k entries the adaptor overhead was
    // measurable. Most names differ within a byte or two, so the loop almost
    // always exits immediately.
    for i in 0..shared {
        let (x, y) = (a[i].to_ascii_lowercase(), b[i].to_ascii_lowercase());
        if x != y {
            return x.cmp(&y);
        }
    }
    a.len().cmp(&b.len())
}

/// Eight bytes of `name` starting at `offset`, lowercased and packed
/// big-endian.
///
/// Every name is known to agree on the bytes before `offset`, so ordering by
/// this agrees exactly with ordering by the lowercased name over those eight
/// bytes. Shorter names sort first because the padding is zero — a byte no
/// filename can contain. Ties are resolved by `cmp_name`.
fn prefix_key(name: &str, offset: usize) -> u64 {
    let bytes = name.as_bytes();
    let mut key = 0u64;
    for slot in offset..offset + 8 {
        let byte = bytes.get(slot).copied().unwrap_or(0);
        key = (key << 8) | byte.to_ascii_lowercase() as u64;
    }
    key
}

/// How many leading bytes every name in the listing has in common, compared
/// case-insensitively to match the sort.
///
/// Capped so a directory full of very long identical prefixes cannot make this
/// pass expensive; beyond the cap the keys simply start at the cap.
fn shared_prefix_len(entries: &Entries) -> usize {
    const CAP: usize = 24;

    if entries.is_empty() {
        return 0;
    }
    let first = entries.name(0).as_bytes();
    let mut shared = first.len().min(CAP);

    for i in 1..entries.len() {
        if shared == 0 {
            break;
        }
        let name = entries.name(i).as_bytes();
        let limit = shared.min(name.len());
        let mut matched = 0;
        while matched < limit && first[matched].eq_ignore_ascii_case(&name[matched]) {
            matched += 1;
        }
        shared = matched;
    }
    shared
}

/// `needle` must already be lowercase.
///
/// Skips to candidate positions by looking for the needle's first byte — in
/// either case — before attempting a full match. Filenames rarely contain the
/// first letter of what you are searching for at many positions, so the inner
/// comparison usually runs once or not at all.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if needle.len() > haystack.len() {
        return false;
    }
    let upper = first.to_ascii_uppercase();

    let last_start = haystack.len() - needle.len();
    let mut start = 0;
    while start <= last_start {
        let candidate = haystack[start];
        if candidate != first && candidate != upper {
            start += 1;
            continue;
        }
        if rest
            .iter()
            .zip(&haystack[start + 1..])
            .all(|(&wanted, &found)| found.to_ascii_lowercase() == wanted)
        {
            return true;
        }
        start += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entries::{Entry, Kind};

    fn listing() -> Entries {
        let mut e = Entries::new();
        for (name, size, modified, kind) in [
            ("Zebra.txt", 10, 300, Kind::File),
            ("apple.txt", 30, 100, Kind::File),
            ("docs", 0, 200, Kind::Directory),
            ("Banana.log", 20, 400, Kind::File),
            ("Archive", 0, 50, Kind::Directory),
        ] {
            e.push(Entry {
                name,
                size,
                modified,
                mode: 0,
                kind,
            });
        }
        e
    }

    fn names(order: &Order, entries: &Entries) -> Vec<String> {
        order.iter().map(|i| entries.name(i).to_owned()).collect()
    }

    #[test]
    fn sorts_directories_first_then_by_name_case_insensitively() {
        let entries = listing();
        let mut order = Order::new();
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(
            names(&order, &entries),
            ["Archive", "docs", "apple.txt", "Banana.log", "Zebra.txt"]
        );
    }

    #[test]
    fn descending_reverses_files_but_keeps_directories_on_top() {
        let entries = listing();
        let mut order = Order::new();
        order.rebuild(
            &entries,
            Sort {
                descending: true,
                ..Sort::default()
            },
            Filter::new("", true),
        );
        assert_eq!(
            names(&order, &entries),
            ["docs", "Archive", "Zebra.txt", "Banana.log", "apple.txt"]
        );
    }

    #[test]
    fn sorting_by_size_and_by_date_uses_the_right_column() {
        let entries = listing();
        let mut order = Order::new();

        order.rebuild(
            &entries,
            Sort {
                key: SortKey::Size,
                directories_first: false,
                descending: false,
            },
            Filter::new("", true),
        );
        assert_eq!(
            names(&order, &entries),
            ["Archive", "docs", "Zebra.txt", "Banana.log", "apple.txt"]
        );

        order.rebuild(
            &entries,
            Sort {
                key: SortKey::Modified,
                directories_first: false,
                descending: false,
            },
            Filter::new("", true),
        );
        assert_eq!(
            names(&order, &entries),
            ["Archive", "apple.txt", "docs", "Zebra.txt", "Banana.log"]
        );
    }

    #[test]
    fn equal_keys_fall_back_to_name_so_the_order_is_deterministic() {
        let entries = listing();
        let mut order = Order::new();
        let sort = Sort {
            key: SortKey::Size,
            directories_first: false,
            descending: false,
        };
        order.rebuild(&entries, sort, Filter::new("", true));
        let first = names(&order, &entries);
        order.rebuild(&entries, sort, Filter::new("", true));
        assert_eq!(first, names(&order, &entries));
        // Both directories have size 0 and must not swap around.
        assert_eq!(&first[..2], ["Archive", "docs"]);
    }

    #[test]
    fn filter_is_a_case_insensitive_substring_match() {
        let entries = listing();
        let mut order = Order::new();

        order.rebuild(&entries, Sort::default(), Filter::new("an", true));
        assert_eq!(names(&order, &entries), ["Banana.log"]);

        // Matches regardless of case and regardless of where in the name it
        // falls — "Zebra.txt" earns its place on the trailing "a".
        order.rebuild(&entries, Sort::default(), Filter::new("A", true));
        assert_eq!(
            names(&order, &entries),
            ["Archive", "apple.txt", "Banana.log", "Zebra.txt"]
        );

        order.rebuild(&entries, Sort::default(), Filter::new(".TXT", true));
        assert_eq!(names(&order, &entries), ["apple.txt", "Zebra.txt"]);
    }

    #[test]
    fn filter_matches_at_the_very_start_and_very_end_of_a_name() {
        let mut entries = Entries::new();
        for name in ["prefix-match", "match-suffix", "exactly"] {
            entries.push(Entry {
                name,
                size: 0,
                modified: 0,
                mode: 0,
                kind: Kind::File,
            });
        }
        let mut order = Order::new();

        order.rebuild(&entries, Sort::default(), Filter::new("match", true));
        assert_eq!(order.len(), 2);

        // The whole name as the needle: the scan must reach the last valid
        // starting position, not stop one short.
        order.rebuild(&entries, Sort::default(), Filter::new("exactly", true));
        assert_eq!(order.len(), 1);

        // One byte longer than the name it nearly matches.
        order.rebuild(&entries, Sort::default(), Filter::new("exactlyy", true));
        assert!(order.is_empty());
    }

    #[test]
    fn filter_survives_many_false_starts() {
        // The needle's first byte appears repeatedly before the real match, so
        // the skip loop has to keep going rather than give up.
        let mut entries = Entries::new();
        entries.push(Entry {
            name: "aaaaaaaaab",
            size: 0,
            modified: 0,
            mode: 0,
            kind: Kind::File,
        });
        let mut order = Order::new();
        order.rebuild(&entries, Sort::default(), Filter::new("aab", true));
        assert_eq!(order.len(), 1);
        order.rebuild(&entries, Sort::default(), Filter::new("aac", true));
        assert!(order.is_empty());
    }

    #[test]
    fn hidden_entries_are_left_out_unless_asked_for() {
        let mut entries = Entries::new();
        for name in [".bashrc", "notes.txt", ".config", "photo.png"] {
            entries.push(Entry {
                name,
                size: 0,
                modified: 0,
                mode: 0,
                kind: Kind::File,
            });
        }
        let mut order = Order::new();

        order.rebuild(&entries, Sort::default(), Filter::new("", false));
        assert_eq!(names(&order, &entries), ["notes.txt", "photo.png"]);

        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(
            names(&order, &entries),
            [".bashrc", ".config", "notes.txt", "photo.png"]
        );
    }

    #[test]
    fn the_filter_and_the_hidden_rule_both_apply() {
        let mut entries = Entries::new();
        for name in [".config", ".conf-backup", "config.toml", "readme.md"] {
            entries.push(Entry {
                name,
                size: 0,
                modified: 0,
                mode: 0,
                kind: Kind::File,
            });
        }
        let mut order = Order::new();

        // Searching for "conf" must not resurrect the hidden ones.
        order.rebuild(&entries, Sort::default(), Filter::new("conf", false));
        assert_eq!(names(&order, &entries), ["config.toml"]);

        order.rebuild(&entries, Sort::default(), Filter::new("conf", true));
        assert_eq!(
            names(&order, &entries),
            [".conf-backup", ".config", "config.toml"]
        );
    }

    #[test]
    fn showing_hidden_entries_does_not_require_re_sorting() {
        // Both are filter decisions, so the cached sort must survive the
        // toggle — that is what keeps it cheap.
        let entries = listing();
        let mut order = Order::new();
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        let before = order.sorted.clone();
        order.rebuild(&entries, Sort::default(), Filter::new("", false));
        assert_eq!(order.sorted, before);
    }

    #[test]
    fn filter_matching_nothing_yields_an_empty_order() {
        let entries = listing();
        let mut order = Order::new();
        order.rebuild(
            &entries,
            Sort::default(),
            Filter::new("no-such-thing", true),
        );
        assert!(order.is_empty());
    }

    #[test]
    fn filter_longer_than_any_name_does_not_panic() {
        let entries = listing();
        let mut order = Order::new();
        order.rebuild(
            &entries,
            Sort::default(),
            Filter::new(&"x".repeat(500), true),
        );
        assert!(order.is_empty());
    }

    #[test]
    fn rebuild_on_an_empty_listing_is_fine() {
        let mut order = Order::new();
        order.rebuild(&Entries::new(), Sort::default(), Filter::new("", true));
        assert!(order.is_empty());
        order.rebuild(&Entries::new(), Sort::default(), Filter::new("abc", true));
        assert!(order.is_empty());
    }

    #[test]
    fn filtering_does_not_disturb_the_sort() {
        let entries = listing();
        let mut order = Order::new();

        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        let unfiltered = names(&order, &entries);

        order.rebuild(&entries, Sort::default(), Filter::new("a", true));
        // Survivors keep their relative order from the sorted list.
        let filtered = names(&order, &entries);
        let expected: Vec<_> = unfiltered
            .iter()
            .filter(|name| name.to_lowercase().contains('a'))
            .cloned()
            .collect();
        assert_eq!(filtered, expected);

        // Clearing gets the original order back untouched.
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(names(&order, &entries), unfiltered);
    }

    #[test]
    fn changing_the_sort_reorders_even_though_the_listing_is_unchanged() {
        let entries = listing();
        let mut order = Order::new();

        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        let by_name = names(&order, &entries);

        order.rebuild(
            &entries,
            Sort {
                key: SortKey::Size,
                directories_first: false,
                descending: false,
            },
            Filter::new("", true),
        );
        assert_ne!(names(&order, &entries), by_name);

        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(names(&order, &entries), by_name);
    }

    #[test]
    fn new_entries_invalidate_the_cached_sort() {
        let mut entries = listing();
        let mut order = Order::new();
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(order.len(), 5);

        entries.push(Entry {
            name: "aaa-first.txt",
            size: 1,
            modified: 1,
            mode: 0,
            kind: Kind::File,
        });
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        assert_eq!(order.len(), 6);
        // It must land in sorted position, not merely be appended.
        let sorted = names(&order, &entries);
        assert_eq!(sorted[2], "aaa-first.txt");
    }

    fn sorted_names(names: &[&str]) -> Vec<String> {
        let mut entries = Entries::new();
        for name in names {
            entries.push(Entry {
                name,
                size: 0,
                modified: 0,
                mode: 0,
                kind: Kind::File,
            });
        }
        let mut order = Order::new();
        order.rebuild(
            &entries,
            Sort {
                directories_first: false,
                ..Sort::default()
            },
            Filter::new("", true),
        );
        order.iter().map(|i| entries.name(i).to_owned()).collect()
    }

    /// The sort keys start after whatever prefix every name shares, so these
    /// cases exercise the offset rather than the front of the name.
    #[test]
    fn names_sharing_a_long_prefix_still_sort_correctly() {
        assert_eq!(
            sorted_names(&[
                "measurement-2026-03.csv",
                "measurement-2026-01.csv",
                "measurement-2026-02.csv",
            ]),
            [
                "measurement-2026-01.csv",
                "measurement-2026-02.csv",
                "measurement-2026-03.csv",
            ]
        );
    }

    #[test]
    fn a_name_that_is_exactly_the_shared_prefix_sorts_first() {
        assert_eq!(
            sorted_names(&["report-b", "report", "report-a"]),
            ["report", "report-a", "report-b"]
        );
    }

    #[test]
    fn the_shared_prefix_is_measured_case_insensitively() {
        // "File-" and "file-" share five bytes once folded, so the keys start
        // at the digits. Getting this wrong would order them by case.
        assert_eq!(
            sorted_names(&["File-3.txt", "file-1.txt", "FILE-2.txt"]),
            ["file-1.txt", "FILE-2.txt", "File-3.txt"]
        );
    }

    #[test]
    fn prefixes_longer_than_the_cap_are_handled() {
        let long = "a".repeat(40);
        assert_eq!(
            sorted_names(&[
                &format!("{long}-c.txt"),
                &format!("{long}-a.txt"),
                &format!("{long}-b.txt"),
            ]),
            [
                format!("{long}-a.txt"),
                format!("{long}-b.txt"),
                format!("{long}-c.txt"),
            ]
        );
    }

    #[test]
    fn identical_names_do_not_break_the_prefix_scan() {
        assert_eq!(sorted_names(&["same", "same", "same"]).len(), 3);
        assert_eq!(sorted_names(&["only"]), ["only"]);
    }

    #[test]
    fn a_single_differing_byte_deep_in_the_name_is_respected() {
        // Beyond both the shared prefix and the eight key bytes, so this can
        // only come out right if `cmp_name` still resolves the tie.
        let stem = "quarterly-financial-summary-";
        assert_eq!(
            sorted_names(&[
                &format!("{stem}zebra.pdf"),
                &format!("{stem}alpha.pdf"),
                &format!("{stem}mango.pdf"),
            ]),
            [
                format!("{stem}alpha.pdf"),
                format!("{stem}mango.pdf"),
                format!("{stem}zebra.pdf"),
            ]
        );
    }

    #[test]
    fn toggling_the_active_column_flips_direction() {
        let mut sort = Sort::default();
        sort.toggle(SortKey::Name);
        assert!(sort.descending);
        sort.toggle(SortKey::Size);
        assert_eq!(sort.key, SortKey::Size);
        assert!(!sort.descending);
    }
}
