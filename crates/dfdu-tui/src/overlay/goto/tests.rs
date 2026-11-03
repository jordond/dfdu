use std::time::Instant;

use dfdu_tree::HostPath;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::model::snapshot::{EntryData, test_entry};
use crate::tests::{press, render, snap_with_totals, typed};
use crate::theme::Theme;

/// `/r`, the scan root, with the directory `lib` and a file, a scan of another host.
fn app() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let lib = EntryData {
        is_dir: true,
        ..test_entry(0, "lib", 3000)
    };
    let mut snap = snap_with_totals(vec![lib, test_entry(1, "notes.txt", 500)], 1);
    snap.root_path = HostPath::from("/r");
    app.refresh(snap, t0);
    (app, t0)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Opens the prompt with `Ctrl-G`, empties it and types `path`.
fn go_to(app: &mut App, path: &str, t0: Instant) {
    app.key(ctrl('g'), t0);
    app.key(ctrl('u'), t0);
    typed(app, path, t0);
}

#[test]
fn ctrl_g_opens_the_prompt_with_the_directory_shown() {
    let (mut app, t0) = app();
    app.key(ctrl('g'), t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert_eq!(p.input.text(), "/r/");
    let frame = render(&mut app, t0, 90, 12);
    assert!(frame.contains("Go to folder"), "{frame}");
    assert!(frame.contains("/r/"), "{frame}");
    app.key(press(KeyCode::Esc), t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.reopen, None);
}

#[test]
fn enter_opens_a_folder_of_the_scan_in_place() {
    let (mut app, t0) = app();
    go_to(&mut app, "lib", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.session.target.path, HostPath::from("/r/lib"));
    assert_eq!(app.reopen, None);
}

#[test]
fn enter_on_a_folder_outside_the_scan_asks_for_a_new_scan() {
    let (mut app, t0) = app();
    go_to(&mut app, "../mnt/disk1", t0);
    app.key(press(KeyCode::Enter), t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.reopen, Some(HostPath::from("/mnt/disk1")));
}

#[test]
fn what_is_wrong_with_the_path_keeps_the_prompt_open() {
    let (mut app, t0) = app();
    app.session.from_file = true;
    go_to(&mut app, "/elsewhere", t0);
    app.key(press(KeyCode::Enter), t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("the prompt closed");
    };
    assert_eq!(p.error.as_deref(), Some(dfdu_session::goto::FROM_FILE));
    assert_eq!(app.reopen, None);
    // Typing clears the error.
    typed(&mut app, "x", t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("the prompt closed");
    };
    assert_eq!(p.error, None);
}

#[test]
fn tab_completes_from_the_folders_on_this_machine() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = dunce::canonicalize(tmp.path()).expect("canonical");
    for d in ["photos", "projects", "music"] {
        std::fs::create_dir(root.join(d)).expect("mkdir");
    }
    let (mut app, t0) = app();
    app.session.outside.local = true;
    app.session.snap.dir_path = HostPath::local(&root);
    app.session.snap.root_path = HostPath::local(&root);
    let sep = char::from(dfdu_tree::Host::LOCAL.sep());
    let text = |app: &App| match &app.overlay {
        Overlay::GoTo(p) => (p.input.text().to_owned(), p.candidates.clone()),
        _ => panic!("no prompt"),
    };

    app.key(ctrl('g'), t0);
    typed(&mut app, "p", t0);
    app.key(press(KeyCode::Tab), t0);
    let (input, candidates) = text(&app);
    assert_eq!(input, format!("{}{sep}p", root.display()));
    assert_eq!(candidates, ["photos", "projects"]);
    let frame = render(&mut app, t0, 90, 14);
    assert!(
        frame.contains("photos") && frame.contains("projects"),
        "{frame}"
    );

    typed(&mut app, "r", t0);
    app.key(press(KeyCode::Tab), t0);
    assert_eq!(
        text(&app).0,
        format!("{}{sep}projects{sep}", root.display())
    );
    assert!(text(&app).1.is_empty());
}

#[test]
fn parent_twice_at_the_root_asks_for_a_scan_of_the_parent() {
    let (mut app, t0) = app();
    app.key(press(KeyCode::Left), t0);
    assert_eq!(app.reopen, None);
    let status = app.session.status.clone().unwrap_or_default();
    assert!(status.contains("press ← again to scan /"), "{status}");
    app.key(press(KeyCode::Backspace), t0);
    assert_eq!(app.reopen, Some(HostPath::from("/")));
}
