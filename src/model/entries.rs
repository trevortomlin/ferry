// A directory listing, stored column-wise.
//
// The obvious representation is `Vec<Entry>` with a `String` name per entry.
// At 100k entries that is 100k separate heap allocations, ~48 bytes of `String`
// and `Vec` overhead each before any actual characters, and a pointer chase per
// row while drawing. Instead every name goes into one growable buffer and each
// column is its own array:
//
//   names   "file-1.datfile-2.dat..."   one allocation
//   ends    [10, 20, ...]               u32 offsets, so a name is a slice
//   sizes   [0, 0, ...]
//   ...
//
// The result is six allocations total regardless of entry count, ~34 bytes of
// fixed cost per entry plus the name bytes, and cache-friendly scans when
// sorting or filtering, which only ever touch one column.

/// What a listing entry is. Kept to one byte so the column stays dense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

impl Kind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// One entry being pushed. Borrowed, so nothing is allocated at the call site.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub size: u64,
    /// Unix seconds. `0` means the server did not report one.
    pub modified: i64,
    /// Unix permission bits. `0` means unknown, which is normal for S3.
    pub mode: u32,
    pub kind: Kind,
}

#[derive(Debug, Default, Clone)]
pub struct Entries {
    names: String,
    /// End offset of each name in `names`; the start is the previous end.
    ends: Vec<u32>,
    sizes: Vec<u64>,
    /// Unix seconds as u32: good until 2106, and 4 bytes a row cheaper than
    /// i64. Times before 1970 are stored as 0, "unknown".
    modified: Vec<u32>,
    /// File type and permission bits, which fit in 16.
    modes: Vec<u16>,
    kinds: Vec<Kind>,
    /// Bumped on every mutation, so `Order` can tell whether its sort is still
    /// valid without comparing contents. Only meaningful against the one
    /// `Entries` it was read from.
    revision: u64,
}

impl Entries {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-size for a listing we already know the length of. Saves the growth
    /// reallocations on the S3 path, where a page size is known up front.
    pub fn with_capacity(count: usize, name_bytes: usize) -> Self {
        Self {
            names: String::with_capacity(name_bytes),
            ends: Vec::with_capacity(count),
            sizes: Vec::with_capacity(count),
            modified: Vec::with_capacity(count),
            modes: Vec::with_capacity(count),
            kinds: Vec::with_capacity(count),
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn push(&mut self, entry: Entry<'_>) {
        self.revision += 1;
        self.names.push_str(entry.name);
        self.ends.push(self.names.len() as u32);
        self.sizes.push(entry.size);
        self.modified
            .push(entry.modified.clamp(0, u32::MAX as i64) as u32);
        self.modes.push(entry.mode as u16);
        self.kinds.push(entry.kind);
    }

    /// Move another listing's entries onto the end of this one. Used to stitch
    /// together the chunks that arrive from a paginated backend.
    pub fn append(&mut self, other: &Self) {
        self.revision += 1;
        let base = self.names.len() as u32;
        self.names.push_str(&other.names);
        self.ends.extend(other.ends.iter().map(|end| end + base));
        self.sizes.extend_from_slice(&other.sizes);
        self.modified.extend_from_slice(&other.modified);
        self.modes.extend_from_slice(&other.modes);
        self.kinds.extend_from_slice(&other.kinds);
    }

    /// Drop the slack left by growing while chunks arrived. Called once a
    /// listing is complete; a 100k listing otherwise holds up to twice what
    /// it needs.
    pub fn shrink_to_fit(&mut self) {
        self.names.shrink_to_fit();
        self.ends.shrink_to_fit();
        self.sizes.shrink_to_fit();
        self.modified.shrink_to_fit();
        self.modes.shrink_to_fit();
        self.kinds.shrink_to_fit();
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Keeps the buffers so a refresh of the same directory reuses them.
    pub fn clear(&mut self) {
        self.revision += 1;
        self.names.clear();
        self.ends.clear();
        self.sizes.clear();
        self.modified.clear();
        self.modes.clear();
        self.kinds.clear();
    }

    pub fn name(&self, i: usize) -> &str {
        let end = self.ends[i] as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.names[start..end]
    }

    pub fn size(&self, i: usize) -> u64 {
        self.sizes[i]
    }

    pub fn modified(&self, i: usize) -> i64 {
        self.modified[i] as i64
    }

    pub fn mode(&self, i: usize) -> u32 {
        self.modes[i] as u32
    }

    pub fn kind(&self, i: usize) -> Kind {
        self.kinds[i]
    }

    /// Heap bytes held. Surfaced so the status bar can show what a listing
    /// actually costs rather than us guessing.
    pub fn heap_bytes(&self) -> usize {
        self.names.capacity()
            + self.ends.capacity() * size_of::<u32>()
            + self.sizes.capacity() * size_of::<u64>()
            + self.modified.capacity() * size_of::<u32>()
            + self.modes.capacity() * size_of::<u16>()
            + self.kinds.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Entry<'_> {
        Entry {
            name,
            size,
            modified: 0,
            mode: 0o644,
            kind: Kind::File,
        }
    }

    fn build(names: &[&str]) -> Entries {
        let mut e = Entries::new();
        for (i, name) in names.iter().enumerate() {
            e.push(file(name, i as u64));
        }
        e
    }

    #[test]
    fn names_round_trip_including_the_first_and_last() {
        let e = build(&["a", "bb", "ccc"]);
        assert_eq!(e.len(), 3);
        assert_eq!(e.name(0), "a");
        assert_eq!(e.name(1), "bb");
        assert_eq!(e.name(2), "ccc");
    }

    #[test]
    fn multibyte_names_are_not_split() {
        let e = build(&["café", "日本語", "⛴"]);
        assert_eq!(e.name(0), "café");
        assert_eq!(e.name(1), "日本語");
        assert_eq!(e.name(2), "⛴");
    }

    #[test]
    fn empty_names_do_not_desynchronise_the_offsets() {
        let e = build(&["", "after", ""]);
        assert_eq!(e.len(), 3);
        assert_eq!(e.name(0), "");
        assert_eq!(e.name(1), "after");
        assert_eq!(e.name(2), "");
    }

    #[test]
    fn append_rebases_offsets_onto_the_target() {
        let mut a = build(&["one", "two"]);
        let b = build(&["three", "four"]);
        a.append(&b);
        assert_eq!(a.len(), 4);
        assert_eq!(a.name(0), "one");
        assert_eq!(a.name(2), "three");
        assert_eq!(a.name(3), "four");
        assert_eq!(a.size(3), 1);
    }

    #[test]
    fn append_onto_empty_works() {
        let mut a = Entries::new();
        a.append(&build(&["x", "y"]));
        assert_eq!(a.name(0), "x");
        assert_eq!(a.name(1), "y");
    }

    #[test]
    fn clear_resets_length_but_reuses_buffers() {
        let mut e = build(&["some", "entries", "here"]);
        let capacity = e.heap_bytes();
        e.clear();
        assert_eq!(e.len(), 0);
        assert!(e.is_empty());
        assert_eq!(e.heap_bytes(), capacity);
        e.push(file("fresh", 1));
        assert_eq!(e.name(0), "fresh");
    }

    #[test]
    fn hundred_thousand_entries_stay_within_a_few_megabytes() {
        let mut e = Entries::new();
        let mut name = String::new();
        for i in 0..100_000u32 {
            name.clear();
            use std::fmt::Write as _;
            let _ = write!(name, "file-{i:06}.dat");
            e.push(file(&name, i as u64));
        }
        assert_eq!(e.len(), 100_000);
        assert_eq!(e.name(99_999), "file-099999.dat");
        // 15 bytes of name plus 33 bytes of columns, so ~4.8 MB before slack.
        assert!(e.heap_bytes() < 8 * 1024 * 1024, "{}", e.heap_bytes());
    }
}
