use std::time::Instant;

use dfdu_session::goto::{Kept, Reach};
use dfdu_tree::HostPath;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

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

/// `p` with `/` for each separator: a Windows host joins with `\`, so tests compare paths in
/// one form on every platform.
fn slashed(p: &HostPath) -> String {
    String::from_utf8_lossy(p.as_bytes()).replace('\\', "/")
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

/// `Enter`, and the run loop going to the path submitted with a tree that has every path
/// inside the scan as a folder.
fn enter(app: &mut App, t0: Instant) {
    enter_with(app, t0, |p| Reach::Dir(p.clone()));
}

/// `Enter`, and the run loop going to the path submitted with `reach` for the tree.
fn enter_with(app: &mut App, t0: Instant, reach: impl FnOnce(&HostPath) -> Reach) {
    app.key(press(KeyCode::Enter), t0);
    if let Some(text) = submitted(app) {
        let gone = session::go(&mut app.session, &text, reach);
        gone_to(app, gone);
    }
}

#[test]
fn ctrl_g_opens_the_prompt_with_the_directory_shown() {
    let (mut app, t0) = app();
    app.key(ctrl('g'), t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert_eq!(p.input.text().replace('\\', "/"), "/r/");
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
    enter(&mut app, t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(slashed(&app.session.target.path), "/r/lib");
    assert_eq!(app.reopen, None);
}

#[test]
fn enter_on_a_folder_outside_the_scan_asks_for_a_new_scan() {
    let (mut app, t0) = app();
    go_to(&mut app, "../mnt/disk1", t0);
    enter(&mut app, t0);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(
        app.reopen.as_ref().map(slashed).as_deref(),
        Some("/mnt/disk1")
    );
}

#[test]
fn what_is_wrong_with_the_path_keeps_the_prompt_open() {
    let (mut app, t0) = app();
    app.session.from_file = Some(session::FROM_FILE);
    go_to(&mut app, "/elsewhere", t0);
    enter(&mut app, t0);
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

#[test]
fn parent_at_the_root_is_disarmed_by_an_overlay_key_or_a_click() {
    let (mut prompted, t0) = app();
    prompted.key(press(KeyCode::Left), t0);
    prompted.key(ctrl('g'), t0);
    prompted.key(press(KeyCode::Esc), t0);
    prompted.key(press(KeyCode::Left), t0);
    assert_eq!(prompted.reopen, None, "the prompt took a key in between");

    let (mut scrolled, t0) = app();
    scrolled.key(press(KeyCode::Left), t0);
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    let _ = crate::input::mouse::event(&mut scrolled, wheel, t0);
    scrolled.key(press(KeyCode::Left), t0);
    assert_eq!(scrolled.reopen, None, "the wheel moved the list in between");

    let (mut late, t0) = app();
    late.key(press(KeyCode::Left), t0);
    late.key(press(KeyCode::Left), t0 + session::ARMED_FOR * 2);
    assert_eq!(late.reopen, None, "too late");
}

#[test]
fn the_notice_of_a_first_parent_goes_with_a_key_a_pane_takes_unless_replaced() {
    // The marks pane takes `↓` but leaves `←` to the list.
    let (mut taken, t0) = app();
    taken.session.marks_pane.open = true;
    taken.key(press(KeyCode::Left), t0);
    assert!(taken.session.up_armed.is_some() && taken.session.status.is_some());
    taken.key(press(KeyCode::Down), t0);
    assert_eq!(taken.session.up_armed, None);
    assert_eq!(taken.session.status, None);

    let (mut replaced, t0) = app();
    replaced.session.marks_pane.open = true;
    replaced.key(press(KeyCode::Left), t0);
    replaced.session.status = Some("copied".into());
    replaced.key(press(KeyCode::Down), t0);
    assert_eq!(replaced.session.status.as_deref(), Some("copied"));
}

#[test]
fn a_folder_the_tree_does_not_have_keeps_the_place_and_says_why() {
    let (mut app, t0) = app();
    let before = app.session.target.path.clone();
    go_to(&mut app, "lib/disk", t0);
    let kept = Kept::NotEntered(HostPath::from("/r/lib"));
    enter_with(&mut app, t0, |_| Reach::Kept(kept.clone()));
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.session.target.path, before);
    assert_eq!(app.session.status, Some(kept.text(true)));
    assert_eq!(app.reopen, None);
}

/// An empty prompt on a scan of this machine lists its mounts once they are read, off the
/// key handler; `↓` picks one and `Tab` puts it in the prompt.
#[test]
fn an_empty_prompt_lists_the_mounts_when_they_arrive() {
    let (mut app, t0) = app();
    app.session.outside.local = true;
    go_to(&mut app, "", t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert!(p.asking, "the key handler does not wait for the mounts");
    let frame = render(&mut app, t0, 90, 14);
    assert!(frame.contains("asking the host"), "{frame}");
    let until = Instant::now() + std::time::Duration::from_secs(10);
    while !app.session.goto.poll() {
        assert!(Instant::now() < until, "the mounts were not read");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(poll(&mut app));
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert!(!p.asking);
    let Some(first) = p.mounts.first().cloned() else {
        // A machine that lists no mounts (a sandbox) has nothing to pick.
        return;
    };
    let frame = render(&mut app, t0, 120, 20);
    assert!(frame.contains(&first.point), "{frame}");
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Tab), t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert_eq!(p.input.text(), first.text(app.session.snap.dir_path.host()));
    assert!(p.mounts.is_empty() && p.pick.is_none());
}

#[test]
fn an_answer_for_another_text_is_not_shown() {
    let (mut app, t0) = app();
    app.session.outside.local = true;
    go_to(&mut app, "", t0);
    typed(&mut app, "/r", t0);
    let Overlay::GoTo(p) = &app.overlay else {
        panic!("no prompt");
    };
    assert!(
        !p.asking && !app.session.goto.pending(),
        "the edit forgot the ask"
    );
    assert!(!poll(&mut app));
}

#[test]
fn an_empty_scan_root_says_how_to_get_out() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut snap = snap_with_totals(Vec::new(), 1);
    snap.dir_path = HostPath::from("/mnt/cache");
    snap.root_path = HostPath::from("/mnt/cache");
    snap.busy = false;
    app.refresh(snap, t0);
    let frame = render(&mut app, t0, 120, 12);
    assert!(
        frame.contains("This folder is empty: press ← twice to scan its parent, or Ctrl-G"),
        "{frame}"
    );
}

#[test]
fn a_folder_gone_to_clears_the_status_line() {
    let (mut app, t0) = app();
    app.session.status = Some("left from before".into());
    go_to(&mut app, "lib", t0);
    enter(&mut app, t0);
    assert_eq!(app.session.status, None);
}
