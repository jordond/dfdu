use std::time::{Duration, Instant};

use ratatui::crossterm::event::KeyCode;

use crate::app::App;
use crate::model::snapshot::{EntryData, test_entry};
use crate::tests::{press, row_names, snap_without_totals};
use crate::theme::Theme;

#[test]
fn a_selection_whose_entry_is_gone_keeps_its_position() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let es = vec![
        test_entry(0, "a", 30),
        test_entry(1, "b", 20),
        test_entry(2, "c", 10),
    ];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Down), t0);
    assert_eq!(app.selected_index(), Some(2));
    // "c" is deleted and an unrelated entry arrives, which waits for a sort: the cursor stays
    // on the last row there is.
    let gone = vec![es[0].clone(), es[1].clone(), test_entry(3, "d", 5)];
    app.refresh(snap_without_totals(gone, 2), t0);
    assert_eq!(app.list.rows.len(), 2);
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"b".to_vec())
    );
}

#[test]
fn cursor_follows_node_across_resort_and_keeps_its_line() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es: Vec<EntryData> = (0..10)
        .map(|i| test_entry(i, &format!("e{i}"), 1000 - u64::from(i) * 10))
        .collect();
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    app.list_height = 5;
    for _ in 0..3 {
        app.key(press(KeyCode::Down), t0);
    }
    app.list.scroll = 1;
    let picked = app.selected.clone().expect("selected").node;
    assert_eq!(app.selected_index(), Some(3));

    // e3 becomes the largest. Within the idle window nothing moves.
    es[3].size = 5000;
    app.refresh(
        snap_without_totals(es.clone(), 2),
        t0 + Duration::from_millis(100),
    );
    assert_eq!(app.selected_index(), Some(3));

    // After the idle delay the automatic sort moves e3 to the top; the cursor follows it
    // and the scroll offset keeps it on the same screen line where possible.
    app.refresh(snap_without_totals(es, 3), t0 + Duration::from_secs(3));
    assert_eq!(app.selected.as_ref().and_then(|s| s.node), picked);
    assert_eq!(app.selected_index(), Some(0));
    assert_eq!(app.list.scroll, 0);
}

#[test]
fn new_entries_wait_for_sort() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 10)];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    es.push(test_entry(1, "b", 99));
    es.push(test_entry(2, "c", 50));
    app.key(press(KeyCode::Down), t0);
    app.refresh(snap_without_totals(es.clone(), 2), t0);
    assert_eq!(app.list.rows.len(), 1);
    assert_eq!((app.list.new_count, app.list.new_largest), (2, 99));
    app.key(press(KeyCode::Char('S')), t0);
    app.refresh(snap_without_totals(es, 3), t0);
    assert_eq!(app.list.rows.len(), 3);
    assert_eq!(app.list.new_count, 0);
}

#[test]
fn finishing_scan_sorts_at_once_when_idle() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 10), test_entry(1, "b", 5)];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    es[1].size = 999;
    let mut done = snap_without_totals(es, 2);
    done.busy = false;
    // 100 ms after the first sort: well inside the minimum interval.
    app.refresh(done, t0 + Duration::from_millis(100));
    assert_eq!(app.snap.entries[app.list.rows[0]].name, b"b");
    // The cursor was never moved, so it goes to the top with the new order.
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"b".to_vec())
    );
}

#[test]
fn rescanned_child_keeps_its_row() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let es = vec![
        test_entry(0, "a", 30),
        test_entry(1, "b", 20),
        test_entry(2, "c", 10),
    ];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    app.key(press(KeyCode::Down), t0);
    let b = app.selected_entry().expect("b").clone();
    // A rescan of "b" swaps in a new node with the same name and identity.
    let b2 = EntryData {
        name: b.name.clone(),
        dev_ino: b.dev_ino,
        ..test_entry(7, "b", 25)
    };
    let new_node = b2.node;
    app.refresh(
        snap_without_totals(vec![es[0].clone(), b2, es[2].clone()], 2),
        t0,
    );
    assert_eq!(app.list.new_count, 0, "the swapped row is not shown as new");
    assert_eq!(app.list.rows.len(), 3);
    assert_eq!(app.selected_index(), Some(1));
    assert_eq!(app.selected.as_ref().and_then(|s| s.node), Some(new_node));
}

#[test]
fn untouched_cursor_stays_at_top_after_resort() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es = vec![test_entry(0, "a", 30), test_entry(1, "b", 20)];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    es[1].size = 900;
    app.refresh(snap_without_totals(es, 2), t0 + Duration::from_secs(3));
    assert_eq!(app.selected_index(), Some(0));
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"b".to_vec())
    );
    assert_eq!(app.list.scroll, 0);
}

/// Six entries a to f, largest first, in a list three rows high.
fn six(app: &mut App, t0: Instant) -> Vec<EntryData> {
    let es: Vec<EntryData> = ["a", "b", "c", "d", "e", "f"]
        .into_iter()
        .zip(0u32..)
        .map(|(name, i)| test_entry(i, name, 600 - u64::from(i) * 100))
        .collect();
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    app.list_height = 3;
    es
}

fn selected_name(app: &App) -> Option<String> {
    app.selected_entry()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
}

#[test]
fn untouched_cursor_keeps_the_first_screen_sorted() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es = six(&mut app, t0);
    // Within the idle window, e and f grow past a and b, and a new entry passes everything.
    let t1 = t0 + Duration::from_millis(100);
    es[4].size = 1000;
    es[5].size = 900;
    es.push(test_entry(6, "g", 2000));
    es.push(test_entry(7, "h", 50));
    app.refresh(snap_without_totals(es, 2), t1);
    // The three rows on screen are sorted at once; the rows below wait for the automatic sort.
    assert_eq!(row_names(&app), ["g", "e", "f", "a", "b", "c", "d"]);
    assert_eq!(app.list.new_count, 1);
    assert_eq!(selected_name(&app).as_deref(), Some("g"));
    assert_eq!(app.list.scroll, 0);
    assert!(app.sort_pending(), "the rows below the screen still wait");
}

#[test]
fn rows_freeze_while_the_cursor_is_off_the_top() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut es = six(&mut app, t0);
    app.key(press(KeyCode::Down), t0);
    let t1 = t0 + Duration::from_millis(100);
    es[4].size = 1000;
    app.refresh(snap_without_totals(es.clone(), 2), t1);
    assert_eq!(row_names(&app), ["a", "b", "c", "d", "e", "f"]);
    assert_eq!(selected_name(&app).as_deref(), Some("b"));

    // Back on the first row, the next snapshot sorts the first screen, and the cursor stays on
    // the entry it is on.
    app.key(press(KeyCode::Up), t1);
    es[5].size = 900;
    app.refresh(snap_without_totals(es, 3), t1);
    assert_eq!(row_names(&app), ["e", "f", "a", "b", "c", "d"]);
    assert_eq!(selected_name(&app).as_deref(), Some("a"));
    assert_eq!(app.selected_index(), Some(2));
}
