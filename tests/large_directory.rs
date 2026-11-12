// What a very large directory costs in the data layer.
//
//   cargo test -p ferry-core --test large_directory --release -- --nocapture
//
// The assertions are deliberately loose — well above what the numbers actually
// are — so this catches a catastrophic regression without failing on a loaded
// machine. The printed figures are the point; they are recorded in
// docs/performance.md.
//
// Two name shapes are measured, because they stress sorting very differently:
//
//   shared prefix   file-000001.dat, file-000002.dat …  (a camera dump, a
//                   sequence export — every comparison must read deep into the
//                   name before it can decide)
//   varied          k3f9-report.pdf, q1zz-notes.txt …   (a normal home
//                   directory — comparisons decide on the first byte or two)

use std::time::Instant;

use ferry::model::browser::Browser;
use ferry::model::entries::{Entries, Entry, Kind};
use ferry::model::message::{Event, SessionId};
use ferry::model::order::{Filter, Order, Sort, SortKey};

const COUNT: usize = 100_000;

fn shared_prefix(i: usize) -> String {
    format!("file-{:06}.dat", (i * 7919) % COUNT)
}

fn varied(i: usize) -> String {
    // Scrambled so neither insertion order nor the first byte is sorted.
    let scrambled = (i * 2_654_435_761) % COUNT;
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut name = String::new();
    let mut n = scrambled;
    for _ in 0..5 {
        name.push(alphabet[n % alphabet.len()] as char);
        n /= alphabet.len();
    }
    name.push_str("-report.pdf");
    name
}

fn build(namer: impl Fn(usize) -> String) -> Entries {
    let mut entries = Entries::with_capacity(COUNT, COUNT * 18);
    for i in 0..COUNT {
        entries.push(Entry {
            name: &namer(i),
            size: (i as u64 * 1237) % 4_000_000,
            modified: 1_700_000_000 + (i as i64 % 86_400),
            mode: 0o100644,
            kind: if i % 10 == 0 {
                Kind::Directory
            } else {
                Kind::File
            },
        });
    }
    entries
}

fn time<T>(label: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let value = work();
    println!(
        "  {label:<38} {:>8.2} ms",
        started.elapsed().as_micros() as f64 / 1000.0
    );
    value
}

#[test]
fn sorting_and_filtering_a_hundred_thousand_entries() {
    for (shape, namer) in [
        ("shared prefix", &shared_prefix as &dyn Fn(usize) -> String),
        ("varied names", &varied),
    ] {
        println!("\n{COUNT} entries, {shape}:");
        let entries = build(namer);
        assert_eq!(entries.len(), COUNT);

        let bytes = entries.heap_bytes();
        println!(
            "  {:<38} {:>8} KiB ({:.1} bytes/entry)",
            "arena size",
            bytes / 1024,
            bytes as f64 / COUNT as f64
        );
        assert!(bytes < 16 * 1024 * 1024, "arena grew to {bytes} bytes");

        let mut order = Order::new();

        time("sort by name", || {
            order.rebuild(&entries, Sort::default(), Filter::new("", true))
        });
        assert_eq!(order.len(), COUNT);

        time("sort by size", || {
            order.rebuild(
                &entries,
                Sort {
                    key: SortKey::Size,
                    ..Sort::default()
                },
                Filter::new("", true),
            )
        });

        // Typing into the filter box, starting from what the user is actually
        // looking at: an already-sorted list. Each keystroke re-runs `rebuild`,
        // so this is the number that decides whether the box feels responsive.
        order.rebuild(&entries, Sort::default(), Filter::new("", true));
        let word = "report";
        time("8 filter keystrokes", || {
            for length in 1..=word.len().min(8) {
                order.rebuild(
                    &entries,
                    Sort::default(),
                    Filter::new(&word[..length], true),
                );
            }
        });
        println!(
            "  {:<38} {:>8}",
            "  survivors after last keystroke",
            order.len()
        );

        // Clearing the filter, which is also a keystroke.
        time("clear the filter", || {
            order.rebuild(&entries, Sort::default(), Filter::new("", true))
        });

        // What one frame of the virtualized list touches.
        let visible = 40;
        time("read 40 visible rows x1000", || {
            let mut total = 0usize;
            for frame in 0..1000 {
                let first = (frame * 37) % (COUNT - visible);
                for row in first..first + visible {
                    let index = order.get(row);
                    total += entries.name(index).len();
                    total += entries.size(index) as usize;
                }
            }
            std::hint::black_box(total)
        });
    }
    println!();
}

/// A large directory does not arrive at once. It arrives in chunks, and the
/// list fills in progressively. This drives `Browser` the way the engine does,
/// so it measures the policy the app actually uses — including how often the
/// order is recomputed while the listing streams in.
#[test]
fn a_chunked_load_of_a_hundred_thousand_entries() {
    const CHUNK: usize = 512;
    const SESSION: SessionId = SessionId(1);
    println!("\n{COUNT} entries arriving in {CHUNK}-entry chunks:");

    let mut browser = Browser::default();
    browser.session = Some(SESSION);
    let generation = browser.navigate("/upload/big");

    let started = Instant::now();
    let mut chunks = 0usize;

    for start in (0..COUNT).step_by(CHUNK) {
        let end = (start + CHUNK).min(COUNT);
        let mut chunk = Entries::with_capacity(CHUNK, CHUNK * 18);
        for i in start..end {
            chunk.push(Entry {
                name: &shared_prefix(i),
                size: i as u64,
                modified: 0,
                mode: 0o100644,
                kind: Kind::File,
            });
        }
        chunks += 1;
        let mut event = Event::Listing {
            session: SESSION,
            generation,
            entries: chunk,
            last: end == COUNT,
        };
        browser.apply(&mut event, Sort::default(), Filter::new("", true));
    }

    let elapsed = started.elapsed();
    println!(
        "  {:<38} {:>8.2} ms  ({chunks} chunks)",
        "total load",
        elapsed.as_micros() as f64 / 1000.0
    );
    assert_eq!(browser.total(), COUNT);
    assert_eq!(browser.visible(), COUNT);
    assert!(
        !browser.loading,
        "the last chunk should have ended the load"
    );

    // Whatever the throttling did mid-stream, the finished list must be fully
    // sorted — the last chunk always forces a reorder.
    let entries = browser.entries();
    let order = browser.order();
    for row in 1..order.len() {
        let (previous, current) = (order.get(row - 1), order.get(row));
        assert!(
            entries.name(previous) <= entries.name(current),
            "row {row} is out of order"
        );
    }
    println!();
    assert!(
        elapsed.as_secs() < 60,
        "a chunked load took {elapsed:?}, which is far past unusable"
    );
}
