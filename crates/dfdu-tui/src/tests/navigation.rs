//! Navigation and marking keys of the list: half pages, the range mark, copying and opening the
//! selected path, and the filter opening its only match.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{library_snap, press, snap_with_totals, typed};
use crate::app::keys::READ_ONLY;
use crate::app::{App, Effect};
use crate::model::snapshot::test_entry;
use crate::theme::Theme;

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// `/r` with the files `f00` (the largest) to `f19`, and room for 10 rows.
fn twenty() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let entries = (0..20)
        .map(|i| test_entry(i, &format!("f{i:02}"), 1000 - u64::from(i)))
        .collect();
    app.refresh(snap_with_totals(entries, 1), t0);
    app.list_height = 10;
    (app, t0)
}

fn marked(app: &App) -> Vec<String> {
    (app.marks.items().iter())
        .map(|it| {
            it.target
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn half_pages_in_the_list() {
    let (mut app, t0) = twenty();
    app.key(ctrl('d'), t0);
    assert_eq!(app.selected_index(), Some(5));
    app.key(ctrl('d'), t0);
    assert_eq!(app.selected_index(), Some(10));
    app.key(ctrl('u'), t0);
    assert_eq!(app.selected_index(), Some(5));
    app.key(ctrl('u'), t0);
    app.key(ctrl('u'), t0);
    assert_eq!(app.selected_index(), Some(0));
}

#[test]
fn range_mark_marks_every_row_passed() {
    let (mut app, t0) = twenty();
    app.key(press(KeyCode::Char('j')), t0);
    app.key(press(KeyCode::Char('v')), t0);
    assert_eq!(marked(&app), ["f01"]);
    assert!(
        app.status
            .as_deref()
            .is_some_and(|s| s.starts_with("range mark"))
    );
    app.key(press(KeyCode::Char('j')), t0);
    app.key(press(KeyCode::Char('j')), t0);
    assert_eq!(marked(&app), ["f01", "f02", "f03"]);
    // Going back marks nothing new; half a page down marks all it passes.
    app.key(press(KeyCode::Char('k')), t0);
    app.key(ctrl('d'), t0);
    assert_eq!(marked(&app).len(), 7, "{:?}", marked(&app));
    assert_eq!(
        app.status.as_deref(),
        Some("range mark: 7 marked; v or Esc ends it")
    );
    // `v` ends it: moving marks no more.
    app.key(press(KeyCode::Char('v')), t0);
    assert_eq!(app.status.as_deref(), Some("range mark ended"));
    app.key(press(KeyCode::Char('j')), t0);
    assert_eq!(marked(&app).len(), 7);
    // `Esc` ends it too, and `G` marks to the end.
    app.key(press(KeyCode::Char('v')), t0);
    app.key(press(KeyCode::Char('G')), t0);
    assert_eq!(marked(&app).len(), 19, "all but f00");
    app.key(press(KeyCode::Esc), t0);
    assert_eq!(app.status.as_deref(), Some("range mark ended"));
    app.key(press(KeyCode::Char('g')), t0);
    assert_eq!(marked(&app).len(), 19);
    assert!(!crate::state::marks::range::on(&app));
}

#[test]
fn range_mark_is_refused_read_only() {
    let (mut app, t0) = twenty();
    app.read_only = true;
    app.key(press(KeyCode::Char('v')), t0);
    assert_eq!(app.status.as_deref(), Some(READ_ONLY));
    assert!(!crate::state::marks::range::on(&app));
    app.key(press(KeyCode::Char('j')), t0);
    assert!(app.marks.is_empty());
}

#[test]
fn y_copies_the_selected_path() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(library_snap(), t0);
    let fx = app.key(press(KeyCode::Char('y')), t0);
    let want = Path::new("/r").join("Library").display().to_string();
    assert!(
        matches!(&fx[..], [Effect::Copy(text)] if *text == want),
        "{fx:?}"
    );
    let status = app.status.clone().unwrap_or_default();
    assert!(
        status.starts_with("copied ") && status.contains("OSC 52"),
        "{status}"
    );
}

#[test]
fn o_opens_the_file_manager_only_for_this_machine() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(library_snap(), t0);
    // A remote scan, or one read from a file: nothing to open here.
    let fx = app.key(press(KeyCode::Char('o')), t0);
    assert!(fx.is_empty(), "{fx:?}");
    assert!(app.status.as_deref().is_some_and(|s| s.contains("remote")));
    app.outside.local = true;
    let fx = app.key(press(KeyCode::Char('o')), t0);
    let want = PathBuf::from("/r").join("Library");
    assert!(
        matches!(&fx[..], [Effect::Open { path, is_dir: true }] if *path == want),
        "{fx:?}"
    );
    app.key(press(KeyCode::Char('j')), t0);
    let fx = app.key(press(KeyCode::Char('o')), t0);
    assert!(
        matches!(&fx[..], [Effect::Open { is_dir: false, .. }]),
        "{fx:?}"
    );
}

#[test]
fn filter_enter_opens_the_only_match() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(library_snap(), t0);
    // Two matches: Enter keeps the filter and stays.
    app.key(press(KeyCode::Char('/')), t0);
    typed(&mut app, "i", t0);
    assert_eq!(app.rows.len(), 2);
    app.key(press(KeyCode::Enter), t0);
    assert!(!app.target.path.ends_with("Library"));
    // One file: stays too.
    app.key(press(KeyCode::Char('/')), t0);
    app.key(ctrl('u'), t0);
    typed(&mut app, "notes", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(!app.target.path.ends_with("Library"));
    // One directory: Enter opens it.
    app.key(press(KeyCode::Char('/')), t0);
    app.key(ctrl('u'), t0);
    typed(&mut app, "libr", t0);
    assert_eq!(app.rows.len(), 1);
    app.key(press(KeyCode::Enter), t0);
    assert_eq!(app.target.path, Path::new("/r").join("Library"));
}
