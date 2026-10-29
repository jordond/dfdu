use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::tests::{press, render};

/// Frame height for these tests: room for the five rows of [`rank_fixture`].
const RANK_H: u16 = 10;

/// The list rows of a frame [`RANK_H`] lines high, without the header, status line and footer
/// (they have arrows of their own).
fn rank_rows(frame: &str) -> Vec<String> {
    frame
        .lines()
        .skip(2)
        .take(RANK_H as usize - 4)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// The rows of `frame` that show a rank marker.
fn with_arrows(frame: &str) -> Vec<String> {
    rank_rows(frame)
        .into_iter()
        .filter(|r| r.contains('↑') || r.contains('↓'))
        .collect()
}

/// Entries a to e (largest first), drawn, with the cursor moved down to row `sel`; then e grows
/// to the largest.
fn rank_fixture(app: &mut App, t0: Instant, sel: usize) -> Vec<EntryData> {
    let mut es: Vec<EntryData> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .zip(0u32..)
        .map(|(name, i)| test_entry(i, name, 500 - u64::from(i) * 100))
        .collect();
    app.refresh(snap_with_alloc(es.clone(), 1), t0);
    render_buffer(app, t0, W, RANK_H);
    for _ in 0..sel {
        app.key(press(KeyCode::Down), t0);
    }
    es[4].size = 1000;
    es
}

#[test]
fn auto_resort_marks_the_selected_row_for_two_seconds() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = rank_fixture(&mut app, t0, 4);
    // After the idle delay and the minimum interval: an automatic re-sort moves e up by four.
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es, 2), t1);
    assert_eq!(row_names(&app), ["e", "a", "b", "c", "d"]);
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"e".to_vec())
    );
    render_buffer(&mut app, t1, W, RANK_H);
    // The bars settle; the marker stays.
    let t = t1 + ms(1500);
    assert!(run_until(&mut app, t1, t) < t1 + Duration::from_secs(1));
    let frame = render(&mut app, t, W, RANK_H);
    insta::assert_snapshot!("rank_markers_mono", frame);
    // The other rows moved down by one each, and are not marked.
    let marked = with_arrows(&frame);
    assert_eq!(marked.len(), 1, "{frame}");
    assert!(marked[0].contains("e  ↑4"), "{frame}");

    // One frame when it expires, then none.
    assert_eq!(app.motion.next_frame_at(), Some(t1 + MARKERS));
    let frame = render(&mut app, t1 + MARKERS, W, RANK_H);
    assert!(with_arrows(&frame).is_empty(), "{frame}");
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn rows_other_than_the_selected_one_get_no_markers() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    // The cursor is on b, which moves down by one; e moves up by four but is not selected.
    let es = rank_fixture(&mut app, t0, 1);
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es, 2), t1);
    assert_eq!(row_names(&app), ["e", "a", "b", "c", "d"]);
    let frame = render(&mut app, t1, W, RANK_H);
    assert!(with_arrows(&frame).is_empty(), "{frame}");
}

#[test]
fn a_selected_row_that_moved_two_places_gets_no_marker() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = rank_fixture(&mut app, t0, 2);
    // c, not e, becomes the largest: it moves up by two.
    es[4].size = 100;
    es[2].size = 1000;
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es, 2), t1);
    assert_eq!(row_names(&app), ["c", "a", "b", "d", "e"]);
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"c".to_vec())
    );
    let frame = render(&mut app, t1, W, RANK_H);
    assert!(with_arrows(&frame).is_empty(), "{frame}");
}

#[test]
fn explicit_sort_shows_no_markers() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = rank_fixture(&mut app, t0, 4);
    // Within the idle window and with the cursor off the top row, nothing moves.
    let t1 = t0 + ms(100);
    app.refresh(snap_with_alloc(es, 2), t1);
    assert_eq!(row_names(&app), ["a", "b", "c", "d", "e"]);
    app.key(press(KeyCode::Char('S')), t1);
    app.reorder(t1);
    assert_eq!(row_names(&app), ["e", "a", "b", "c", "d"]);
    let frame = render(&mut app, t1, W, RANK_H);
    assert!(with_arrows(&frame).is_empty(), "{frame}");
    run_until(&mut app, t1, t1 + Duration::from_secs(5));
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn rank_markers_survive_a_resort_that_replaced_the_entries() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = rank_fixture(&mut app, t0, 4);
    // The re-sort comes with a snapshot whose entries are in another order (a rescan).
    es.reverse();
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap_with_alloc(es, 2), t1);
    assert_eq!(row_names(&app), ["e", "a", "b", "c", "d"]);
    let frame = render(&mut app, t1, W, RANK_H);
    let marked = with_arrows(&frame);
    assert_eq!(marked.len(), 1, "{frame}");
    assert!(marked[0].contains("e  ↑4"), "{frame}");
}
