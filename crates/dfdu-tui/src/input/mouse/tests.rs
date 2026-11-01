use dfdu_tree::HostPath;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::*;
use crate::model::snapshot::{EntryData, test_entry};
use crate::tests::{press, render, snap_without_totals};
use crate::theme::Theme;

mod header;
mod overlays;
mod wheel;

/// The list's first row is on this screen line, below the two header lines and the column
/// names.
const TOP: u16 = 3;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn left(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

fn dir(i: u32, name: &str, size: u64) -> EntryData {
    EntryData {
        is_dir: true,
        ..test_entry(i, name, size)
    }
}

/// A drawn browser of `/r` with directories `d0` (largest) to `d9`.
fn browser(t0: Instant) -> App {
    let mut app = App::new(Theme::no_color(), t0);
    let entries = (0..10)
        .map(|i| dir(i, &format!("d{i}"), 1000 - u64::from(i) * 10))
        .collect();
    app.refresh(snap_without_totals(entries, 1), t0);
    render(&mut app, t0, 80, 12);
    app
}

fn selected(app: &App) -> Option<String> {
    app.session
        .selected_entry()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
}

#[test]
fn click_selects_row() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    assert_eq!(selected(&app).as_deref(), Some("d0"));
    let fx = event(&mut app, left(30, TOP + 2), t0).expect("redraw");
    assert!(fx.is_empty());
    assert_eq!(selected(&app).as_deref(), Some("d2"));
    assert_eq!(
        app.session.target.path,
        HostPath::default(),
        "a single click opens nothing"
    );
    // Clicks below the last row and on the footer change nothing.
    assert!(event(&mut app, left(30, 10), t0).is_none());
    assert!(event(&mut app, left(30, 11), t0).is_none());
    assert_eq!(selected(&app).as_deref(), Some("d2"));
}

#[test]
fn click_on_new_entries_row_sorts() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut entries = vec![dir(0, "a", 10)];
    app.refresh(snap_without_totals(entries.clone(), 1), t0);
    entries.push(dir(1, "b", 99));
    app.refresh(snap_without_totals(entries, 2), t0);
    render(&mut app, t0, 80, 12);
    assert_eq!(app.session.list.new_count, 1);
    event(&mut app, left(10, TOP + 1), t0).expect("redraw");
    app.reorder(t0);
    assert_eq!(app.session.list.new_count, 0);
    assert_eq!(app.session.list.rows.len(), 2);
}

#[test]
fn double_click_opens_directory() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    event(&mut app, left(30, TOP + 1), t0).expect("redraw");
    let t1 = t0 + Duration::from_millis(250);
    event(&mut app, left(30, TOP + 1), t1).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from("/r").join(b"d1"));
}

#[test]
fn double_click_opens_the_clicked_row_after_the_selection_moved() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    event(&mut app, left(30, TOP + 1), t0).expect("redraw");
    app.key(press(KeyCode::Down), t0);
    assert_eq!(selected(&app).as_deref(), Some("d2"));
    let t1 = t0 + Duration::from_millis(250);
    event(&mut app, left(30, TOP + 1), t1).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from("/r").join(b"d1"));
}

#[test]
fn slow_second_click_does_not_open() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    event(&mut app, left(30, TOP + 1), t0).expect("redraw");
    let t1 = t0 + Duration::from_millis(450);
    event(&mut app, left(30, TOP + 1), t1).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from(""));
    assert_eq!(selected(&app).as_deref(), Some("d1"));
    // Two quick clicks on different rows select the second one.
    event(&mut app, left(30, TOP + 3), t1).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from(""));
    assert_eq!(selected(&app).as_deref(), Some("d3"));
}

#[test]
fn mouse_moves_do_not_redraw() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut entries = vec![dir(0, "a", 10)];
    app.refresh(snap_without_totals(entries.clone(), 1), t0);
    entries.push(dir(1, "b", 99));
    app.refresh(snap_without_totals(entries, 2), t0);
    render(&mut app, t0, 80, 12);
    let due = app.session.next_sort_at().expect("a re-sort is pending");
    let later = t0 + Duration::from_secs(1);
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        assert!(event(&mut app, mouse(kind, 30, TOP), later).is_none());
    }
    assert_eq!(
        app.session.next_sort_at(),
        Some(due),
        "not counted as input"
    );
    assert_eq!(selected(&app).as_deref(), Some("a"));
    // A click is input: the re-sort waits for the user to be idle again.
    event(&mut app, left(30, TOP), later).expect("redraw");
    assert!(app.session.next_sort_at().expect("still pending") > due);
}
