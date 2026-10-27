use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::tests::{key, render};

#[test]
fn auto_resort_shows_rank_markers_for_two_seconds() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = resort_fixture(&mut app, t0);
    // After the idle delay and the minimum interval: an automatic re-sort.
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    render_buffer(&mut app, t1, W, H);
    // The bars settle; the markers stay.
    let t = t1 + ms(1500);
    assert!(run_until(&mut app, t1, t) < t1 + Duration::from_secs(1));
    let frame = render(&mut app, t, W, H);
    insta::assert_snapshot!("rank_markers_mono", frame);
    let rows = list_rows(&frame);
    assert!(rows.contains("c  ↑2"), "{frame}");
    assert!(rows.contains("a  ↓1"), "{frame}");
    assert!(rows.contains("b  ↓1"), "{frame}");

    // One frame when they expire, then none.
    assert_eq!(app.motion.next_frame_at(), Some(t1 + MARKERS));
    let frame = render(&mut app, t1 + MARKERS, W, H);
    let rows = list_rows(&frame);
    assert!(!rows.contains('↑') && !rows.contains('↓'), "{frame}");
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn explicit_sort_shows_no_markers() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let es = resort_fixture(&mut app, t0);
    let t1 = t0 + ms(100);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["a", "b", "c"]);
    app.key(key(KeyCode::Char('S')), t1);
    app.reorder(t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    let frame = render(&mut app, t1, W, H);
    let rows = list_rows(&frame);
    assert!(!rows.contains('↑') && !rows.contains('↓'), "{frame}");
    run_until(&mut app, t1, t1 + Duration::from_secs(5));
    assert_eq!(app.motion.next_frame_at(), None);
}

#[test]
fn rank_markers_survive_a_resort_that_replaced_the_entries() {
    let t0 = Instant::now();
    let mut app = animated(Theme::no_color(), t0);
    let mut es = resort_fixture(&mut app, t0);
    // The re-sort comes with a snapshot whose entries are in another order (a rescan).
    es.reverse();
    let t1 = t0 + Duration::from_secs(3);
    app.refresh(snap(es, 2), t1);
    assert_eq!(names(&app), ["c", "a", "b"]);
    let rows = list_rows(&render(&mut app, t1, W, H));
    assert!(rows.contains("c  ↑2"), "{rows}");
    assert!(rows.contains("a  ↓1"), "{rows}");
    assert!(rows.contains("b  ↓1"), "{rows}");
}
