//! Navigation and marking keys of the list: half pages, the range mark, copying and opening the
//! selected path, and the filter opening its only match.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dfdu_tree::{Host, HostPath, Kind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    below, library_snap, marked_names as marked, meta, press, render, snap_with_totals, typed,
};
use crate::app::keys::READ_ONLY;
use crate::app::{App, Effect};
use crate::features::treemap::tests::sync;
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

/// A scan of another kind of host keeps that host's paths: `C:\Users\u\proj` on a Unix
/// machine (or `/home/u/pro\j`, one name, on a Windows one) is shown, copied and found again
/// as it is.
#[test]
fn paths_of_another_kind_of_host_are_shown_and_found_as_they_are() {
    let (host, root_name, name, want) = match Host::LOCAL {
        Host::Unix => (Host::Windows, r"C:\Users\u", "proj", r"C:\Users\u\proj"),
        Host::Windows => (Host::Unix, "/home/u", r"pro\j", r"/home/u/pro\j"),
    };
    let mut m = dfdu_engine::Model::default();
    m.tree.set_host(host);
    let root = (m.tree).new_root(root_name.as_bytes(), &meta(Kind::Dir, 0, 1), true);
    let dir = (m.tree).add_dir(root, name.as_bytes(), &meta(Kind::Dir, 0, 2));
    let path = HostPath::new(host, want.as_bytes().to_vec());
    assert_eq!(m.tree.host_path(dir), path);
    assert_eq!(dfdu_tree::find_path(&m.tree, &path), Some(dir));

    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &m, t0);
    assert!(render(&mut app, t0, 80, 8).contains(&format!(" {root_name} ")));
    let fx = app.key(press(KeyCode::Char('y')), t0);
    assert!(
        matches!(&fx[..], [Effect::Copy(text)] if *text == want),
        "{fx:?}"
    );
    assert!(app.status.as_deref().is_some_and(|s| s.contains(want)));

    // Gone to by its path: its directory opens with it selected.
    let root_path = app.snap.root_path.clone();
    app.go_to(&path, &root_path);
    sync(&mut app, &m, t0);
    assert_eq!(app.snap.dir_path, root_path);
    assert_eq!(
        app.selected_entry().map(|e| &e.name[..]),
        Some(name.as_bytes())
    );
    app.key(press(KeyCode::Enter), t0);
    sync(&mut app, &m, t0);
    assert_eq!(app.snap.dir_path, path);
    // `render` shows `\` as `/` on Windows, `pro\j` included.
    let header = render(&mut app, t0, 80, 8);
    let want = format!("{root_name} › {name}");
    let want = if cfg!(windows) {
        want.replace('\\', "/")
    } else {
        want
    };
    assert!(header.contains(&want), "{header}");
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
    assert!(!app.target.path.to_local().ends_with("Library"));
    // One file: stays too.
    app.key(press(KeyCode::Char('/')), t0);
    app.key(ctrl('u'), t0);
    typed(&mut app, "notes", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(!app.target.path.to_local().ends_with("Library"));
    // One directory: Enter opens it.
    app.key(press(KeyCode::Char('/')), t0);
    app.key(ctrl('u'), t0);
    typed(&mut app, "libr", t0);
    assert_eq!(app.rows.len(), 1);
    app.key(press(KeyCode::Enter), t0);
    assert_eq!(app.target.path, below("/r", "Library"));
}
