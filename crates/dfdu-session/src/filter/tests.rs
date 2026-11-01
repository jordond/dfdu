use std::time::{Duration, Instant};

use super::*;
use crate::snapshot::test_entry;
use crate::state::tests::{browser, sort};

fn dir(i: u32, name: &str, size: u64) -> EntryData {
    EntryData {
        is_dir: true,
        ..test_entry(i, name, size)
    }
}

fn entries() -> Vec<EntryData> {
    let gib = 1 << 30;
    vec![
        dir(0, "Library", 112 * gib),
        dir(1, "dev", 78 * gib),
        dir(2, ".android", 51 * gib),
        dir(3, "Downloads", 9 * gib),
        test_entry(4, "notes.sqlite", 625 << 20),
        dir(5, "lib64", 300 << 20),
        test_entry(6, "build.log", 12 << 10),
    ]
}

fn row_names(s: &SessionState) -> Vec<String> {
    (s.list.rows.iter())
        .map(|&i| String::from_utf8_lossy(&s.snap.entries[i].name).into_owned())
        .collect()
}

/// Types `text` into the filter, bringing the rows up to date after each edit as a front end
/// does.
fn typed(s: &mut SessionState, text: &str) {
    for c in text.chars() {
        assert_eq!(edit(s, TextOp::Insert(c)), Edited::Refilter);
        let before = selected_node(s);
        sort(s);
        refiltered(s, before);
    }
}

#[test]
fn inactive_filter_counts_nothing_until_opened() {
    let mut f = Filter::default();
    f.prepare(&browser(entries()).snap, true);
    assert_eq!(f.hits, [] as [bool; 0]);
    assert_eq!(f.counts.entries, 0);

    let mut s = browser(entries());
    assert_eq!(s.filter.counts.entries, 0);
    open(&mut s);
    assert_eq!(s.filter.counts.entries, 7);
    assert_eq!(s.filter.counts.matched, 7);
    assert_eq!(s.filter.counts.hidden, 1);
}

#[test]
fn edits_say_what_the_front_end_does_next() {
    let mut s = browser(entries());
    open(&mut s);
    assert!(editing(&s));
    typed(&mut s, "dev");
    assert_eq!(row_names(&s), ["dev"]);
    assert_eq!(edit(&mut s, TextOp::Left), Edited::Done);
    // The only row left is a directory: it is selected for the front end to open.
    s.selected = None;
    assert_eq!(edit(&mut s, TextOp::Submit), Edited::Enter);
    assert!(!editing(&s));
    assert_eq!(selected_node(&s), Some(s.snap.entries[1].node));
    assert_eq!(s.filter.query(), "dev", "the filter is kept");

    open(&mut s);
    assert_eq!(edit(&mut s, TextOp::Cancel), Edited::Done);
    assert!(!editing(&s));
    assert!(s.filter.input().is_empty(), "Esc clears the query");
}

#[test]
fn the_entry_a_query_hid_is_selected_again_when_it_shows() {
    let mut s = browser(entries());
    s.move_to(4);
    let notes = selected_node(&s);
    open(&mut s);
    typed(&mut s, "li");
    assert_eq!(row_names(&s), ["Library", "notes.sqlite", "lib64"]);
    typed(&mut s, "b");
    assert_ne!(selected_node(&s), notes, "the query hid it");
    assert_eq!(edit(&mut s, TextOp::Backspace), Edited::Refilter);
    let before = selected_node(&s);
    sort(&mut s);
    refiltered(&mut s, before);
    assert_eq!(selected_node(&s), notes, "back as it shows again");
}

/// `n` entries with names like `data-0000042-report.json`.
fn synthetic(n: usize) -> Snapshot {
    const WORDS: [&str; 8] = [
        "cache",
        "build",
        "report",
        "data",
        "node_modules",
        "image",
        "Video",
        "tmp",
    ];
    const EXTS: [&str; 5] = ["json", "log", "png", "rs", "tar.gz"];
    let base = test_entry(0, "x", 1);
    let entries = (0..n)
        .map(|i| EntryData {
            name: format!(
                "{}-{i:07}-{}.{}",
                WORDS[i % 8],
                WORDS[(i / 8) % 8],
                EXTS[i % 5]
            )
            .as_bytes()
            .into(),
            ..base.clone()
        })
        .collect();
    Snapshot {
        entries,
        ..Default::default()
    }
}

/// Times `prepare` for a one-letter query and then for a second letter appended.
fn prepare_times(n: usize) {
    let snap = synthetic(n);
    let mut f = Filter::default();
    f.prepare(&snap, true);
    let mut times = Vec::new();
    for q in ["e", "ep"] {
        f.input.set(q);
        let t = Instant::now();
        f.prepare(&snap, false);
        times.push(t.elapsed());
    }
    let hits = f.hits.iter().filter(|&&h| h).count();
    println!("prepare on {n} entries: {times:?}, {hits} match \"ep\"");
    assert!(hits > 0 && hits < n);
    for t in times {
        assert!(t < Duration::from_millis(250), "{t:?}");
    }
}

#[test]
fn filtering_a_large_directory_is_fast() {
    prepare_times(100_000);
}

#[test]
#[ignore = "builds a million entries; run in release. On an M4 Max, release: prepare 21-23 ms"]
fn filtering_a_large_directory_is_fast_at_a_million_entries() {
    prepare_times(1_000_000);
}
