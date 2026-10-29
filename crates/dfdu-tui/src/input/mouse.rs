//! Mouse input: clicks, double clicks and the wheel, mapped through `App::areas`.
//!
//! A click selects a list row (or treemap tile) and a second click on it within [`DOUBLE_CLICK`]
//! opens it; a click
//! on the "new entries" row sorts, one on a breadcrumb goes to its directory, and one outside an
//! open overlay closes it. The delete dialog ignores the mouse, so a stray click never confirms
//! or cancels a delete. The wheel moves the selection, or scrolls the details pane. Over the
//! debug panel, which hides the rows below it, clicks and the wheel do nothing. Pointer movement
//! changes nothing and is not input: it draws no frame and does not hold off the automatic
//! re-sort.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::app::keys::{self, Action};
use crate::app::{App, Effect, Overlay};
use crate::features::details;

/// A second click on the same row within this long opens it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows one wheel step moves.
const WHEEL_ROWS: i8 = 3;

#[derive(Clone, Debug, Default)]
pub struct Mouse {
    /// When the last click on a list row was, and the entry on that row.
    last_click: Option<(Instant, NodeRef)>,
    /// The breadcrumbs the last frame drew, except the current directory's.
    pub crumbs: Vec<Crumb>,
}

/// A breadcrumb on screen. A click goes to its directory and selects `child`, the directory
/// below it on the way to the current one.
#[derive(Clone, Debug)]
pub struct Crumb {
    pub area: Rect,
    pub child: PathBuf,
}

/// Handles a mouse event; `None` when it changed nothing, so no frame is drawn.
pub fn event(app: &mut App, m: MouseEvent, now: Instant) -> Option<Vec<Effect>> {
    let at = Position::new(m.column, m.row);
    let up = match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.note_input(now);
            return click(app, at, now);
        }
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return None,
    };
    app.note_input(now);
    if !matches!(app.overlay, Overlay::None) || over_debug(app, at) {
        return None;
    }
    let step = if up {
        -i32::from(WHEEL_ROWS)
    } else {
        i32::from(WHEEL_ROWS)
    };
    if (app.areas.details.is_some_and(|r| r.contains(at)) || details::focused(app))
        && details::scroll(app, step)
    {
        return Some(Vec::new());
    }
    if app.rows.is_empty() {
        return None;
    }
    app.status = None;
    let rows = isize::from(WHEEL_ROWS);
    app.move_by(if up { -rows } else { rows });
    Some(Vec::new())
}

fn over_debug(app: &App, at: Position) -> bool {
    app.areas.debug.is_some_and(|r| r.contains(at))
}

fn click(app: &mut App, at: Position, now: Instant) -> Option<Vec<Effect>> {
    match app.overlay {
        Overlay::None => {}
        Overlay::ConfirmDelete(_) => return None,
        _ if app.areas.overlay.is_some_and(|r| r.contains(at)) => return None,
        _ => {
            app.overlay = Overlay::None;
            return Some(Vec::new());
        }
    }
    if over_debug(app, at) {
        return None;
    }
    if let Some(c) = app.mouse.crumbs.iter().find(|c| c.area.contains(at)) {
        let (child, root) = (c.child.clone(), app.snap.root_path.clone());
        app.status = None;
        app.mouse.last_click = None;
        app.go_to(&child, &root);
        return Some(Vec::new());
    }
    let list = app.areas.list;
    if !list.contains(at) {
        return None;
    }
    if crate::features::treemap::active(app) {
        app.mouse.last_click = None;
        return crate::features::treemap::click(app, at, now);
    }
    // The rows as the list drew them: entries from `scroll`, then the "new entries" row.
    let line = (at.y - list.y) as usize;
    let new_row = app.new_count > 0;
    let entries = app
        .rows
        .len()
        .saturating_sub(app.scroll)
        .min((list.height as usize).saturating_sub(usize::from(new_row)));
    app.status = None;
    if line < entries {
        let row = app.scroll + line;
        let node = app.snap.entries[app.rows[row]].node;
        let again = app
            .mouse
            .last_click
            .is_some_and(|(t, n)| n == node && now.duration_since(t) <= DOUBLE_CLICK);
        if again {
            app.mouse.last_click = None;
            // The selection may have moved since the first click; open the row clicked.
            app.move_to(row);
            return Some(keys::run(app, Action::Open, now));
        }
        app.mouse.last_click = Some((now, node));
        app.move_to(row);
        return Some(Vec::new());
    }
    if line == entries && new_row {
        app.mouse.last_click = None;
        return Some(keys::run(app, Action::Resort, now));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::model::snapshot::{EntryData, Snapshot, test_entry};
    use crate::tests::{press, render, snap_without_totals};
    use crate::theme::Theme;
    use dfdu_engine::ErrorList;

    /// The list's first row is on this screen line, below the two header lines.
    const TOP: u16 = 2;

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
            Path::new(""),
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
        assert_eq!(app.target.path, Path::new(""));
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
        assert_eq!(app.target.path, Path::new("/r/d1"));
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
        assert_eq!(app.target.path, Path::new("/r/d1"));
    }

    #[test]
    fn slow_second_click_does_not_open() {
        let t0 = Instant::now();
        let mut app = browser(t0);
        event(&mut app, left(30, TOP + 1), t0).expect("redraw");
        let t1 = t0 + Duration::from_millis(450);
        event(&mut app, left(30, TOP + 1), t1).expect("redraw");
        assert_eq!(app.target.path, Path::new(""));
        assert_eq!(selected(&app).as_deref(), Some("d1"));
        // Two quick clicks on different rows select the second one.
        event(&mut app, left(30, TOP + 3), t1).expect("redraw");
        assert_eq!(app.target.path, Path::new(""));
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
        // " dfdu  /r › a › b": `/r` at columns 7-8, `a` at 12.
        assert_eq!(crumbs, [Rect::new(7, 0, 2, 1), Rect::new(12, 0, 1, 1)]);
        // The current directory and the separators are not targets.
        assert!(event(&mut app, left(16, 0), t0).is_none());
        assert!(event(&mut app, left(10, 0), t0).is_none());
        event(&mut app, left(12, 0), t0).expect("redraw");
        assert_eq!(app.target.path, Path::new("/r/a"));
        assert_eq!(app.selected.as_ref().expect("selection").name, b"b");

        let mut app = below_root(t0);
        event(&mut app, left(8, 0), t0).expect("redraw");
        assert_eq!(app.target.path, Path::new("/r"));
        assert_eq!(app.selected.as_ref().expect("selection").name, b"a");
    }
}
