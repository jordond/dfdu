use std::time::Duration;

use dfdu_tree::HostPath;

use super::*;
use crate::snapshot::test_entry;
use crate::view::View;

/// A snapshot of `/r` while it is scanned, holding `entries`.
fn scanning(entries: Vec<EntryData>, version: u64) -> Snapshot {
    Snapshot {
        dir_path: HostPath::from("/r"),
        dir_node: Some(test_entry(99, "/r", 0).node),
        at_root: true,
        entries,
        busy: true,
        version,
        ..Default::default()
    }
}

fn row_names(s: &SessionState) -> Vec<String> {
    (s.list.rows.iter())
        .map(|&i| String::from_utf8_lossy(&s.snap.entries[i].name).into_owned())
        .collect()
}

#[test]
fn a_moved_cursor_follows_its_row_across_a_resort() {
    let t0 = Instant::now();
    let mut s = SessionState::new(t0);
    let mut es: Vec<EntryData> = (0..10)
        .map(|i| test_entry(i, &format!("e{i}"), 1000 - u64::from(i) * 10))
        .collect();
    let _ = s.refresh(scanning(es.clone(), 1), t0, false);
    s.set_viewport(View::Files, 5);
    s.move_to(3);
    // e3 becomes the largest. Within the idle window nothing moves.
    es[3].size = 5000;
    let r = s.refresh(
        scanning(es.clone(), 2),
        t0 + Duration::from_millis(100),
        false,
    );
    assert!(r.resorted.is_none());
    assert_eq!((r.scroll, s.selected_index()), (Scroll::Keep, Some(3)));
    // After the idle delay the automatic sort moves e3 to the top, and the front end's
    // scroll follows it.
    let r = s.refresh(scanning(es, 3), t0 + Duration::from_secs(3), false);
    assert_eq!(s.selected_index(), Some(0));
    assert_eq!(r.scroll, Scroll::Follow { from: 3, to: 0 });
    let resorted = r.resorted.expect("a re-sort");
    assert!(resorted.auto);
    assert_eq!(resorted.before.len(), 10);
}

#[test]
fn the_scroll_keeps_the_selected_row_on_its_line() {
    // Row 7 was on the third line (scroll 5); moved to row 4, the list scrolls to 2.
    assert_eq!(Scroll::Follow { from: 7, to: 4 }.apply(5), 2);
    assert_eq!(Scroll::Follow { from: 2, to: 8 }.apply(0), 6);
    assert_eq!(Scroll::Follow { from: 3, to: 0 }.apply(1), 0);
    assert_eq!((Scroll::Top.apply(9), Scroll::Keep.apply(9)), (0, 9));
}

#[test]
fn an_untouched_cursor_keeps_the_first_screen_sorted() {
    let t0 = Instant::now();
    let mut s = SessionState::new(t0);
    let mut es: Vec<EntryData> = ["a", "b", "c", "d", "e", "f"]
        .into_iter()
        .zip(0u32..)
        .map(|(name, i)| test_entry(i, name, 600 - u64::from(i) * 100))
        .collect();
    let _ = s.refresh(scanning(es.clone(), 1), t0, false);
    s.set_viewport(View::Files, 3);
    // Within the idle window, e and f grow past a and b, and a new entry passes everything.
    let t1 = t0 + Duration::from_millis(100);
    es[4].size = 1000;
    es[5].size = 900;
    es.push(test_entry(6, "g", 2000));
    let held = s.refresh(scanning(es.clone(), 2), t1, true);
    assert_eq!(
        row_names(&s),
        ["a", "b", "c", "d", "e", "f"],
        "rows held still are not sorted"
    );
    assert_eq!(held.scroll, Scroll::Keep);
    let r = s.refresh(scanning(es, 3), t1, false);
    // The three rows on screen are sorted at once; the rows below wait for the automatic sort.
    assert_eq!(row_names(&s), ["g", "e", "f", "a", "b", "c", "d"]);
    assert_eq!(r.scroll, Scroll::Top);
    assert!(r.resorted.is_none());
    assert!(s.sort_pending());
}
