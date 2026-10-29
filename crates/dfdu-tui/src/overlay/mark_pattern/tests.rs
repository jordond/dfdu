use std::time::Instant;

use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::model::snapshot::{EntryData, test_entry};
use crate::tests::{press, render, snap_with_totals, typed};
use crate::theme::Theme;

/// `/r` with two logs, a directory of logs and a text file.
fn app() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let entries = vec![
        test_entry(0, "build.log", 3000),
        test_entry(1, "server.log", 2000),
        EntryData {
            is_dir: true,
            ..test_entry(2, "old.log", 1000)
        },
        test_entry(3, "notes.txt", 500),
    ];
    app.refresh(snap_with_totals(entries, 1), t0);
    (app, t0)
}

/// Opens the prompt from the palette, as the user does.
fn open_from_palette(app: &mut App, t0: Instant) {
    app.key(press(KeyCode::Char(':')), t0);
    typed(app, "mark by pattern", t0);
    app.key(press(KeyCode::Enter), t0);
}

fn marked(app: &App) -> Vec<String> {
    let mut names: Vec<String> = (app.marks.items().iter())
        .map(|it| {
            it.target
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn marks_the_names_the_glob_matches() {
    let (mut app, t0) = app();
    open_from_palette(&mut app, t0);
    assert!(matches!(app.overlay, Overlay::MarkPattern(_)));
    typed(&mut app, "*.log", t0);
    let frame = render(&mut app, t0, 80, 12);
    assert!(frame.contains("3 of 4 names here match"), "{frame}");
    insta::assert_snapshot!("mark_pattern_80x12", frame);
    app.key(press(KeyCode::Enter), t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(marked(&app), ["build.log", "old.log", "server.log"]);
    let status = app.status.clone().unwrap_or_default();
    assert!(
        status.starts_with("marked 3 of 3 entries matching *.log"),
        "{status}"
    );

    // Again: nothing new to mark. `?` matches one character.
    open_from_palette(&mut app, t0);
    typed(&mut app, "*.log", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(
        app.status
            .as_deref()
            .is_some_and(|s| s.contains("marked already"))
    );
    crate::state::marks::clear(&mut app);
    open_from_palette(&mut app, t0);
    typed(&mut app, "notes.tx?", t0);
    app.key(press(KeyCode::Enter), t0);
    assert_eq!(marked(&app), ["notes.txt"]);
}

#[test]
fn a_bad_or_empty_glob_marks_nothing() {
    let (mut app, t0) = app();
    open_from_palette(&mut app, t0);
    typed(&mut app, "[ab]*", t0);
    let frame = render(&mut app, t0, 80, 12);
    assert!(frame.contains("`[...]` is not supported"), "{frame}");
    app.key(press(KeyCode::Enter), t0);
    assert!(app.marks.is_empty());
    assert!(
        app.status
            .as_deref()
            .is_some_and(|s| s.contains("not supported"))
    );

    open_from_palette(&mut app, t0);
    typed(&mut app, "*.iso", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(app.marks.is_empty());
    assert_eq!(app.status.as_deref(), Some("nothing here matches *.iso"));

    // Esc closes the prompt without marking.
    open_from_palette(&mut app, t0);
    typed(&mut app, "*", t0);
    app.key(press(KeyCode::Esc), t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert!(app.marks.is_empty());
}

#[test]
fn read_only_does_not_open_it() {
    let (mut app, t0) = app();
    app.read_only = true;
    open_from_palette(&mut app, t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.status.as_deref(), Some(READ_ONLY));
    // Nor does anything else mark through the bulk path.
    let items = vec![marks::item_of(&app, &app.snap.entries[0])];
    assert_eq!(marks::add(&mut app, items), 0);
    assert!(app.marks.is_empty());
}
