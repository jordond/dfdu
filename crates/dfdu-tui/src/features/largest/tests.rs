use std::time::Instant;

use dfdu_engine::Model;
use dfdu_engine::test_support::{add_dir, add_file};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::*;
use crate::app::Overlay;
use crate::app::keys::READ_ONLY;
use crate::tests::sync;
use crate::tests::{press, refused, render, root_model};
use crate::theme::Theme;

/// `/r` with files at three depths, the largest in a hidden directory:
/// `.cache/huge` 9000, `a/big.iso` 5000, `top.txt` 4000, `a/deep/x.bin` 3000.
fn model() -> Model {
    let (mut m, root) = root_model();
    let a = add_dir(&mut m.tree, root, "a", 2);
    let deep = add_dir(&mut m.tree, a, "deep", 3);
    let cache = add_dir(&mut m.tree, root, ".cache", 4);
    add_file(&mut m.tree, deep, "x.bin", 3000, 10);
    add_file(&mut m.tree, a, "big.iso", 5000, 11);
    add_file(&mut m.tree, root, "top.txt", 4000, 12);
    add_file(&mut m.tree, cache, "huge", 9000, 13);
    m
}

fn paths(app: &App) -> Vec<String> {
    (app.largest.files.iter())
        .map(|f| f.path.to_string().replace('\\', "/"))
        .collect()
}

fn opened(m: &Model) -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, m, t0);
    app.key(press(KeyCode::Char('5')), t0);
    assert!(active(&app));
    assert!(stale(&app), "opening asks for a walk");
    sync(&mut app, m, t0);
    (app, t0)
}

#[test]
fn lists_the_largest_files_below_with_their_paths_relative() {
    let m = model();
    let (mut app, t0) = opened(&m);
    assert_eq!(
        paths(&app),
        [".cache/huge", "a/big.iso", "top.txt", "a/deep/x.bin"]
    );
    assert_eq!(app.largest.count, 4);
    let frame = render(&mut app, t0, 100, 10);
    assert!(
        frame.contains("the largest 4 of 4 files below /r"),
        "{frame}"
    );
    assert!(!frame.contains("Library"), "the list is not drawn: {frame}");
    insta::assert_snapshot!("largest_100x10", frame);

    // Hiding hidden files walks again without them.
    app.key(press(KeyCode::Char('e')), t0);
    assert!(stale(&app));
    sync(&mut app, &m, t0);
    assert_eq!(paths(&app), ["a/big.iso", "top.txt", "a/deep/x.bin"]);

    // Below another directory, paths are relative to it. With `.cache` hidden, the filter
    // leaves `a` alone, and Enter opens it.
    app.key(press(KeyCode::Char('1')), t0);
    assert!(!active(&app));
    app.key(press(KeyCode::Char('/')), t0);
    crate::tests::typed(&mut app, "a", t0);
    app.key(press(KeyCode::Enter), t0);
    sync(&mut app, &m, t0);
    assert!(
        app.session.snap.dir_path.to_local().ends_with("a"),
        "{:?}",
        app.session.snap.dir_path
    );
    app.key(press(KeyCode::Char('5')), t0);
    sync(&mut app, &m, t0);
    assert_eq!(paths(&app), ["big.iso", "deep/x.bin"]);
}

#[test]
fn a_directory_without_files_has_an_empty_state() {
    let (mut m, root) = root_model();
    add_dir(&mut m.tree, root, "empty", 2);
    let (mut app, t0) = opened(&m);
    let frame = render(&mut app, t0, 100, 20);
    assert!(frame.contains("╭ Largest ─"), "{frame}");
    assert!(frame.contains("No files below /r."), "{frame}");
    assert!(frame.contains("parent directory"), "{frame}");
}

#[test]
fn enter_goes_to_the_file_in_its_directory() {
    let m = model();
    let (mut app, t0) = opened(&m);
    app.key(press(KeyCode::End), t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(!active(&app), "the view closes");
    assert_eq!(
        app.session.target.path,
        HostPath::from("/r").join(b"a").join(b"deep")
    );
    sync(&mut app, &m, t0);
    let e = app.session.selected_entry().expect("selected");
    assert_eq!(&e.name[..], b"x.bin");
    // `5` opens the view again, now for this directory; `1` comes back to the list.
    app.key(press(KeyCode::Char('5')), t0);
    sync(&mut app, &m, t0);
    assert_eq!(paths(&app), ["x.bin"]);
    app.key(press(KeyCode::Char('1')), t0);
    assert!(!active(&app));
    assert_eq!(
        &app.session.selected_entry().expect("selected").name[..],
        b"x.bin"
    );
}

/// The walk runs only while the view is open, and again only when the data changed.
#[test]
fn walks_only_while_open_and_when_something_changed() {
    let mut m = model();
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &m, t0);
    assert!(read(&m, &app).is_none(), "closed: no walk");
    app.key(press(KeyCode::Char('5')), t0);
    assert!(read(&m, &app).is_some());
    sync(&mut app, &m, t0);
    assert!(read(&m, &app).is_none(), "nothing changed");
    let root = m.tree.roots().next().expect("root");
    add_file(&mut m.tree, root, "new.bin", 20_000, 99);
    sync(&mut app, &m, t0);
    assert_eq!(paths(&app)[0], "new.bin", "no scan runs: walked at once");
}

#[test]
fn keys_move_mark_and_refuse() {
    let m = model();
    let (mut app, t0) = opened(&m);
    render(&mut app, t0, 100, 10);
    // Half a page of the 4 rows shown.
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL), t0);
    assert_eq!(app.largest.cursor, 2);
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), t0);
    assert_eq!(app.largest.cursor, 0);
    // Without marks, `d` deletes nothing: not the list's selection hidden behind the view.
    refused(
        &mut app,
        KeyCode::Char('d'),
        "tick items with Space first",
        t0,
    );
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.session.marks.items().len(), 1);
    let huge = HostPath::from("/r").join(b".cache").join(b"huge");
    assert_eq!(app.session.marks.items()[0].target.path, huge);
    assert_eq!(app.largest.cursor, 1);
    let frame = render(&mut app, t0, 100, 10);
    let row = frame
        .lines()
        .find(|l| l.contains(".cache/huge"))
        .unwrap_or_default();
    assert!(row.starts_with(" [x] "), "{frame}");
    assert!(
        frame.contains("Selected 8.8 KiB of 20.5 KiB · 1 of 4 files"),
        "{frame}"
    );
    app.key(press(KeyCode::Char('d')), t0);
    assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
    app.key(press(KeyCode::Esc), t0);
    // `X` unticks every file.
    app.key(press(KeyCode::Char('X')), t0);
    assert!(app.session.marks.is_empty());

    // A read-only browser marks nothing.
    let (mut ro, t0) = opened(&m);
    ro.session.read_only = true;
    ro.key(press(KeyCode::Char(' ')), t0);
    assert!(ro.session.marks.is_empty());
    assert_eq!(ro.session.status.as_deref(), Some(READ_ONLY));
}
