//! What tree mode touches: marks, the filter, deleted rows, the treemap and the selection line.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::keys::{key, select, selected, small, tree_app};
use super::sync;
use crate::app::App;
use crate::model::snapshot::Snapshot;
use crate::state::motion;
use crate::tests::{render, row_names, typed};
use crate::theme::Theme;

fn marked(app: &App) -> Vec<PathBuf> {
    app.marks
        .items()
        .iter()
        .map(|it| it.target.path.clone())
        .collect()
}

#[test]
fn a_directory_and_what_is_inside_it_are_never_both_marked() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = tree_app(&fx, t0);
    key(&mut app, &fx, KeyCode::Right, t0);
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert_eq!(marked(&app), [Path::new("/r/a")]);
    select(&mut app, "a1");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert_eq!(
        marked(&app),
        [Path::new("/r/a/a1")],
        "the directory is unmarked"
    );
    select(&mut app, "a2");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert_eq!(marked(&app).len(), 2);
    select(&mut app, "a");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert_eq!(
        marked(&app),
        [Path::new("/r/a")],
        "what is inside is unmarked"
    );

    // A range keeps the outermost.
    select(&mut app, "a");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert!(app.marks.is_empty());
    select(&mut app, "a1");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    select(&mut app, "a");
    for code in [KeyCode::Char('v'), KeyCode::Down, KeyCode::Down] {
        key(&mut app, &fx, code, t0);
    }
    assert_eq!(marked(&app), [Path::new("/r/a")]);
}

#[test]
fn in_the_list_a_marked_directory_still_holds_marks() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    select(&mut app, "a");
    key(&mut app, &fx, KeyCode::Enter, t0);
    select(&mut app, "a1");
    key(&mut app, &fx, KeyCode::Char(' '), t0);
    assert_eq!(marked(&app), [Path::new("/r/a"), Path::new("/r/a/a1")]);
}

#[test]
fn the_filter_matches_below_the_top_and_keeps_the_rows_above() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = tree_app(&fx, t0);
    for code in [KeyCode::Right, KeyCode::Right, KeyCode::Right] {
        key(&mut app, &fx, code, t0);
    }
    key(&mut app, &fx, KeyCode::Char('/'), t0);
    typed(&mut app, "x", t0);
    assert_eq!(row_names(&app), ["a", "a1", "x"]);
    // A fold row's text is not a name.
    typed(&mut app, "\u{8}", t0);
    app.key(crate::tests::press(KeyCode::Backspace), t0);
    typed(&mut app, "more", t0);
    assert!(row_names(&app).is_empty(), "{:?}", row_names(&app));
    app.key(crate::tests::press(KeyCode::Esc), t0);
    sync(&mut app, &fx.m, t0);
    assert_eq!(row_names(&app).len(), 7);
}

/// `snap` without its entry named `name`, the indices of the rows after it moved up.
fn without(snap: &Snapshot, name: &str) -> Snapshot {
    let gone = snap
        .entries
        .iter()
        .position(|e| e.name[..] == *name.as_bytes())
        .expect("present");
    let mut out = snap.clone();
    out.entries.remove(gone);
    for e in &mut out.entries {
        if e.parent != u32::MAX && e.parent as usize > gone {
            e.parent -= 1;
        }
    }
    out
}

#[test]
fn a_deleted_row_below_the_top_stays_in_place_until_the_next_sort() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = tree_app(&fx, t0);
    motion::start(&mut app);
    for code in [KeyCode::Right, KeyCode::Right, KeyCode::Right] {
        key(&mut app, &fx, code, t0);
    }
    let rows = ["a", "a1", "x", "a2", "… 1 more", "b", "top"];
    assert_eq!(row_names(&app), rows);
    // `x` is deleted: it is the first entry below the top, so every later one moves up.
    let mut next = without(&app.snap, "x");
    next.deleting = Some((0, 1));
    next.version += 1;
    app.refresh(next.clone(), t0);
    assert_eq!(row_names(&app), rows, "the ghost keeps its place under a1");
    let ghost = app.selected_entry().filter(|e| e.ghost);
    assert!(ghost.is_none() || selected(&app) == "x");
    let frame = render(&mut app, t0, 80, 12);
    let line = frame.lines().find(|l| l.contains(" x ")).expect("x");
    assert!(line.contains("deleted") && line.contains("└─"), "{frame}");
    // And again with the next snapshot, until a sort drops it.
    next.version += 1;
    app.refresh(next, t0);
    assert_eq!(row_names(&app), rows);
    key(&mut app, &fx, KeyCode::Char('S'), t0);
    assert!(!app.snap.entries.iter().any(|e| e.ghost));
}

#[test]
fn the_treemap_shows_the_directory_of_the_selected_row() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = tree_app(&fx, t0);
    key(&mut app, &fx, KeyCode::Right, t0);
    select(&mut app, "a2");
    key(&mut app, &fx, KeyCode::Char('2'), t0);
    assert_eq!(app.target.path, Path::new("/r/a"));
    assert_eq!(selected(&app), "a2");
    assert!(app.tree_shown().is_none(), "the tiles are one directory's");
    assert_eq!(row_names(&app), ["a1", "a2", "s"]);
    // Back in the list, the rows are a tree again.
    key(&mut app, &fx, KeyCode::Char('1'), t0);
    assert!(app.tree_shown().is_some());
    assert_eq!(selected(&app), "a2");
}

#[test]
fn the_selection_line_says_where_the_row_is() {
    let fx = small();
    let t0 = Instant::now();
    let mut app = tree_app(&fx, t0);
    key(&mut app, &fx, KeyCode::Right, t0);
    select(&mut app, "a1");
    let frame = render(&mut app, t0, 80, 12);
    let status = frame.lines().nth(10).expect("selection line");
    assert!(
        status.starts_with(" ▸ /r/a/a1/  500 B · 62.4% of a/ · 3 items"),
        "{frame}"
    );
    assert!(status.ends_with("2/6"), "{frame}");
    // When the whole path does not fit, the path from the scan root.
    let frame = render(&mut app, t0, 22, 12);
    let status = frame.lines().nth(10).expect("selection line");
    assert!(status.starts_with(" ▸ a/a1/  500 B"), "{frame}");
    select(&mut app, "b");
    let frame = render(&mut app, t0, 80, 12);
    assert!(frame.contains("18.2% of r/"), "{frame}");
}
