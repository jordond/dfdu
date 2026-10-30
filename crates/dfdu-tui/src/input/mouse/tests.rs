use dfdu_tree::HostPath;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::*;
use crate::model::snapshot::{EntryData, Snapshot, test_entry};
use crate::tests::{press, render, snap_without_totals};
use crate::theme::Theme;
use dfdu_engine::ErrorList;

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
    app.selected_entry()
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
        app.target.path,
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
    assert_eq!(app.new_count, 1);
    event(&mut app, left(10, TOP + 1), t0).expect("redraw");
    app.reorder(t0);
    assert_eq!(app.new_count, 0);
    assert_eq!(app.rows.len(), 2);
}

#[test]
fn clicks_and_wheel_over_the_debug_panel_change_nothing() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    crate::features::debug::toggle(&mut app);
    render(&mut app, t0, 100, 20);
    let panel = app.areas.debug.expect("debug panel");
    let (x, y) = (panel.x + 2, TOP + 1);
    assert!(panel.contains(Position::new(x, y)));
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
    ] {
        assert!(event(&mut app, mouse(kind, x, y), t0).is_none(), "{kind:?}");
    }
    assert_eq!(selected(&app).as_deref(), Some("d0"));
    assert_eq!(app.target.path, HostPath::from(""));
    // Left of the panel, the same row takes the click.
    event(&mut app, left(panel.x - 1, y), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d1"));
}

#[test]
fn double_click_opens_directory() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    event(&mut app, left(30, TOP + 1), t0).expect("redraw");
    let t1 = t0 + Duration::from_millis(250);
    event(&mut app, left(30, TOP + 1), t1).expect("redraw");
    assert_eq!(app.target.path, HostPath::from("/r").join(b"d1"));
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
    assert_eq!(app.target.path, HostPath::from("/r").join(b"d1"));
}

#[test]
fn slow_second_click_does_not_open() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    event(&mut app, left(30, TOP + 1), t0).expect("redraw");
    let t1 = t0 + Duration::from_millis(450);
    event(&mut app, left(30, TOP + 1), t1).expect("redraw");
    assert_eq!(app.target.path, HostPath::from(""));
    assert_eq!(selected(&app).as_deref(), Some("d1"));
    // Two quick clicks on different rows select the second one.
    event(&mut app, left(30, TOP + 3), t1).expect("redraw");
    assert_eq!(app.target.path, HostPath::from(""));
    assert_eq!(selected(&app).as_deref(), Some("d3"));
}

#[test]
fn wheel_moves_selection() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    let down = mouse(MouseEventKind::ScrollDown, 30, TOP + 4);
    event(&mut app, down, t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d3"));
    for _ in 0..5 {
        event(&mut app, down, t0);
    }
    assert_eq!(selected(&app).as_deref(), Some("d9"));
    event(&mut app, mouse(MouseEventKind::ScrollUp, 30, TOP), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d6"));
}

#[test]
fn wheel_over_the_details_pane_leaves_the_list_alone() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    app.key(press(KeyCode::Char('i')), t0);
    render(&mut app, t0, 120, 12);
    let pane = app.areas.details.expect("the pane is shown");
    let list = app.areas.list;
    let down = |r: Rect| mouse(MouseEventKind::ScrollDown, r.x + 1, r.y + 1);
    event(&mut app, down(pane), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d0"));
    event(&mut app, down(list), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d3"));
    // With the pane focused the wheel scrolls it wherever the pointer is.
    app.key(press(KeyCode::Tab), t0);
    render(&mut app, t0, 120, 12);
    event(&mut app, down(list), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d3"));
    // A click on the list still selects with the pane open.
    event(&mut app, left(list.x + 1, TOP + 5), t0).expect("redraw");
    assert_eq!(selected(&app).as_deref(), Some("d5"));
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
    let due = app.next_sort_at().expect("a re-sort is pending");
    let later = t0 + Duration::from_secs(1);
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        assert!(event(&mut app, mouse(kind, 30, TOP), later).is_none());
    }
    assert_eq!(app.next_sort_at(), Some(due), "not counted as input");
    assert_eq!(selected(&app).as_deref(), Some("a"));
    // A click is input: the re-sort waits for the user to be idle again.
    event(&mut app, left(30, TOP), later).expect("redraw");
    assert!(app.next_sort_at().expect("still pending") > due);
}

#[test]
fn click_outside_overlay_closes_it() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    app.show_errors(ErrorList::default());
    render(&mut app, t0, 80, 12);
    let r = app.areas.overlay.expect("the overlay's area");
    assert!(event(&mut app, left(r.x + 1, r.y + 1), t0).is_none());
    assert!(matches!(app.overlay, Overlay::Errors { .. }));
    let before = selected(&app);
    event(&mut app, left(r.x.saturating_sub(1), TOP + 3), t0).expect("redraw");
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(selected(&app), before, "the click only closed the overlay");
}

#[test]
fn click_never_touches_the_delete_dialog() {
    let t0 = Instant::now();
    let mut app = browser(t0);
    app.key(press(KeyCode::Char('D')), t0);
    render(&mut app, t0, 80, 12);
    assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    let r = app.areas.overlay.expect("the dialog's area");
    let wheel = mouse(MouseEventKind::ScrollDown, 0, TOP);
    for m in [left(0, 0), left(0, TOP + 5), left(r.x + 1, r.y + 1), wheel] {
        assert!(event(&mut app, m, t0).is_none());
        assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    }
    assert_eq!(selected(&app).as_deref(), Some("d0"));
}

/// A drawn browser of `/r/a/b`, below the scan root `/r`.
fn below_root(t0: Instant) -> App {
    let mut app = App::new(Theme::no_color(), t0);
    let below = Snapshot {
        dir_path: "/r/a/b".into(),
        root_path: "/r".into(),
        at_root: false,
        ..snap_without_totals(vec![dir(0, "c", 10)], 1)
    };
    app.refresh(below, t0);
    render(&mut app, t0, 80, 12);
    app
}

#[test]
fn click_on_breadcrumb_goes_up() {
    let t0 = Instant::now();
    let mut app = below_root(t0);
    let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.area).collect();
    // The scope line " /r › a › b": `/r` at columns 1-2, `a` at 6.
    assert_eq!(crumbs, [Rect::new(1, 1, 2, 1), Rect::new(6, 1, 1, 1)]);
    // The current directory and the separators are not targets.
    assert!(event(&mut app, left(10, 1), t0).is_none());
    assert!(event(&mut app, left(4, 1), t0).is_none());
    event(&mut app, left(6, 1), t0).expect("redraw");
    assert_eq!(app.target.path, HostPath::from("/r").join(b"a"));
    assert_eq!(app.selected.as_ref().expect("selection").name, b"b");

    let mut app = below_root(t0);
    event(&mut app, left(2, 1), t0).expect("redraw");
    assert_eq!(app.target.path, HostPath::from("/r"));
    assert_eq!(app.selected.as_ref().expect("selection").name, b"a");
}

#[test]
fn click_on_a_tab_switches_to_its_view() {
    use crate::app::views::{View, current};
    let t0 = Instant::now();
    let mut app = browser(t0);
    let tab = |app: &App, view| {
        let t = app.mouse.tabs.iter().find(|t| t.view == view);
        t.expect("the tab is drawn").area
    };
    assert_eq!(app.mouse.tabs.len(), 5);
    let largest = tab(&app, View::Largest);
    assert_eq!(largest.y, 0);
    event(&mut app, left(largest.right() - 1, 0), t0).expect("redraw");
    assert_eq!(current(&app), View::Largest);
    render(&mut app, t0, 80, 12);
    let files = tab(&app, View::Files);
    event(&mut app, left(files.x, 0), t0).expect("redraw");
    assert_eq!(current(&app), View::Files);
    // The title and the scan state are not targets.
    assert!(event(&mut app, left(2, 0), t0).is_none());
    assert!(event(&mut app, left(79, 0), t0).is_none());
}
