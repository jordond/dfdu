use std::time::Instant;

use dfdu_scan::Budget;
use dfdu_tree::{DirState, ErrKind, Meta, NodeFlags};
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::debug::Counters;
use crate::extras;
use crate::snapshot::{self, Target};
use crate::tests::{assert_cue, press, render, render_buffer};
use crate::theme::Theme;

const MTIME: i64 = 1_792_834_920;

/// [`crate::tests::meta`] modified at `MTIME`.
fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
    Meta {
        mtime: MTIME,
        ..crate::tests::meta(kind, size, ino)
    }
}

/// `/r` holds `proj/`, partly read, with six files, a directory that could not be read, a
/// mount and a read-only subvolume, and data also used outside the scan; and `notes.txt`.
fn model() -> Model {
    let mut m = Model::default();
    let tree = &mut m.tree;
    let root = tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let own = tree.own(root);
    tree.propagate_add(root, &own);
    let dir = |tree: &mut Tree, parent, name: &[u8], ino, flags| {
        let d = tree.add_dir_flagged(parent, name, &meta(Kind::Dir, 0, ino), flags);
        let own = tree.own(d);
        tree.propagate_add(d, &own);
        tree.dir_mut(d).expect("dir").state = DirState::Complete;
        d
    };
    let file = |tree: &mut Tree, parent, name: &[u8], size, ino| {
        let f = tree.add_file(parent, name, &meta(Kind::File, size, ino), false, None);
        let own = tree.own(f);
        tree.propagate_add(parent, &own);
    };
    tree.dir_mut(root).expect("root").state = DirState::Complete;
    let proj = dir(tree, root, b"proj", 2, 0);
    for (i, (name, size)) in [
        ("big.iso", 50_000),
        ("a.o", 12_000),
        ("b.o", 8_000),
        ("c.txt", 3_000),
        ("d.txt", 1_000),
        ("e.txt", 500),
    ]
    .into_iter()
    .enumerate()
    {
        file(tree, proj, name.as_bytes(), size, 10 + i as u64);
    }
    let locked = dir(tree, proj, b"locked", 3, 0);
    tree.dir_mut(locked).expect("dir").state = DirState::Failed;
    tree.set_error(locked, ErrKind::PermissionDenied);
    dir(tree, proj, b"mnt", 4, NodeFlags::MOUNT_ROOT);
    dir(tree, proj, b"snap", 5, NodeFlags::RO_SUBVOL);
    tree.add_home(proj, 6_000, true);
    tree.dir_mut(proj).expect("dir").state = DirState::Partial;
    // Not an `Os` code: its message is the platform's (EIO is "access is denied" on Windows).
    tree.set_error(proj, ErrKind::Other);
    file(tree, root, b"notes.txt", 2_000, 20);
    m
}

/// The browser on `/r` with the details pane open and read.
fn app_on(m: &Model) -> App {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let snap = snapshot::build(m, &mut Target::root(), SizeMode::Disk);
    app.refresh(snap, t0);
    assert!(app.key(press(KeyCode::Char('i')), t0).is_empty());
    assert!(stale(&app));
    read_extras(m, &mut app);
    app
}

fn read_extras(m: &Model, app: &mut App) {
    let x = extras::read(m, app, &Budget::new(1 << 20), Counters::default());
    extras::apply(app, x);
    assert!(!stale(app));
}

/// A read with a snapshot, as the run loop does it: the extras are read while the app still
/// holds the previous snapshot, and taken in after the new one.
fn read_full(m: &Model, app: &mut App) {
    let x = extras::read(m, app, &Budget::new(1 << 20), Counters::default());
    let snap = snapshot::build(m, &mut Target::root(), SizeMode::Disk);
    app.refresh(snap, app.started);
    extras::apply(app, x);
}

/// The child `name` of `/r`.
fn child(m: &Model, name: &[u8]) -> Ix {
    let tree = &m.tree;
    let root = tree.roots().next().expect("root");
    tree.children(root)
        .find(|&c| tree.name(c) == name)
        .expect("child")
}

#[test]
fn a_snapshot_read_updates_the_scan_state_and_compression() {
    let mut m = model();
    let proj = child(&m, b"proj");
    m.tree.dir_mut(proj).expect("dir").state = DirState::Pending;
    let mut app = app_on(&m);
    let t0 = app.started;
    let out = render(&mut app, t0, 120, 20);
    assert!(out.contains("directory · scanning"), "{out}");
    // The scan of the selected directory ends with an error: the same node, another mark.
    m.tree.dir_mut(proj).expect("dir").state = DirState::Partial;
    read_full(&m, &mut app);
    assert!(!stale(&app));
    let out = render(&mut app, t0, 120, 20);
    assert!(
        out.contains("directory · incomplete") && !out.contains("directory · scanning"),
        "{out}"
    );

    app.key(press(KeyCode::Down), t0);
    read_extras(&m, &mut app);
    assert!(!render(&mut app, t0, 120, 20).contains("(compressed)"));
    let notes = child(&m, b"notes.txt");
    m.tree.set_flag(notes, NodeFlags::COMPRESSED, true);
    read_full(&m, &mut app);
    assert!(!stale(&app));
    let out = render(&mut app, t0, 120, 20);
    assert!(
        out.contains("2.0 KiB on disk at most (compressed)"),
        "{out}"
    );
}

#[test]
fn every_mark_has_a_scan_state() {
    for (mark, want) in [
        (Mark::None, "complete"),
        (Mark::ErrorsInside, "complete, errors below"),
        (Mark::Scanning, "scanning"),
        (Mark::Stalled, "stalled: not responding"),
        (Mark::Rescanning, "rescanning"),
        (Mark::Error, "incomplete"),
        (Mark::Incomplete, "incomplete: scan stopped, r to rescan"),
        (Mark::Deleting, "being deleted"),
        (Mark::OtherFs, "not scanned: other filesystem"),
        (Mark::DupMount, "not scanned: mounted again"),
        (Mark::Network, "not scanned: network filesystem"),
        (Mark::Kernel, "not scanned: kernel filesystem"),
    ] {
        assert_eq!(state(mark), want, "{mark:?}");
    }
}

#[test]
fn errors_below_a_complete_directory_read_dim_like_its_size() {
    let mut m = model();
    let tree = &mut m.tree;
    let root = tree.roots().next().expect("root");
    let logs = tree.add_dir_flagged(root, b"logs", &meta(Kind::Dir, 0, 30), 0);
    let own = tree.own(logs);
    tree.propagate_add(logs, &own);
    tree.dir_mut(logs).expect("dir").state = DirState::Complete;
    let big = meta(Kind::File, 100_000, 31);
    let f = tree.add_file(logs, b"big.log", &big, false, None);
    let own = tree.own(f);
    tree.propagate_add(logs, &own);
    let locked = tree.add_dir_flagged(logs, b"locked", &meta(Kind::Dir, 0, 32), 0);
    let own = tree.own(locked);
    tree.propagate_add(locked, &own);
    tree.dir_mut(locked).expect("dir").state = DirState::Failed;
    tree.set_error(locked, ErrKind::PermissionDenied);
    let mut app = app_on(&m);
    let e = app.selected_entry().expect("selected");
    assert_eq!((&e.name[..], e.mark), (&b"logs"[..], Mark::ErrorsInside));
    app.theme = Theme::ansi16();
    let t0 = app.started;
    let buf = render_buffer(&mut app, t0, 120, 20);
    assert!(render(&mut app, t0, 120, 20).contains("directory · complete, errors below"));
    let dim = app.theme.fg_dim.fg;
    assert!(dim.is_some());
    assert_cue(&buf, "complete, errors below", |s| s.fg == dim);
}

#[test]
fn details_pane_shows_tree_facts() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let out = render(&mut app, t0, 120, 20);
    insta::assert_snapshot!("details_right_mono", out);
    for want in [
        "╭ proj/ ",
        "/r/proj",
        "directory · incomplete",
        "78.6 KiB on disk · 11 items",
        "72.8 KiB apparent · 72.8 KiB like du",
        "5.9 KiB also used outside the scan:",
        "partly read: could not be read",
        "1 error below",
        "largest by disk usage",
        "48.8 KiB  big.iso",
        "2.9 KiB  c.txt",
        "1000 B  d.txt",
    ] {
        assert!(out.contains(want), "{want:?} is not in\n{out}");
    }
    // The five largest of nine children: the rest are left out.
    assert!(!out.contains("e.txt") && !out.contains("locked/"), "{out}");
    assert!(scroll(&mut app, 100));
    let end = render(&mut app, t0, 120, 20);
    for want in [
        "modified 2026-10-24 09:42 UTC",
        "device 1 · inode 2",
        "inside: 1 mount, 1 read-only",
        "subvolume; deletion stops at them",
    ] {
        assert!(end.contains(want), "{want:?} is not in\n{end}");
    }
}

#[test]
fn details_pane_is_a_bottom_sheet_when_narrow() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let out = render(&mut app, t0, 80, 24);
    insta::assert_snapshot!("details_bottom_mono", out);
    let pane = app.areas.details.expect("pane");
    assert_eq!((pane.width, pane.height), (80, SHEET_HEIGHT));
    assert_eq!(app.areas.list.height, 20 - SHEET_HEIGHT);
}

#[test]
fn details_pane_hidden_when_too_small() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let out = render(&mut app, t0, 60, 12);
    assert!(app.areas.details.is_none());
    assert!(!out.contains("╭ proj/"), "{out}");
    assert!(out.contains(TOO_SMALL), "{out}");
    // Tab cannot focus a pane that is not shown, and the wheel scrolls the list instead.
    app.key(press(KeyCode::Tab), t0);
    assert!(!focused(&app));
    assert!(!scroll(&mut app, 1));
    let out = render(&mut app, t0, 120, 20);
    assert!(out.contains("╭ proj/") && !out.contains(TOO_SMALL), "{out}");
}

#[test]
fn too_small_does_not_replace_another_message() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    app.status = Some("deleted 3 entries".into());
    let out = render(&mut app, t0, 60, 12);
    assert!(app.areas.details.is_none());
    assert!(
        out.contains("deleted 3 entries") && !out.contains(TOO_SMALL),
        "{out}"
    );
    app.status = None;
    assert!(render(&mut app, t0, 60, 12).contains(TOO_SMALL));
}

#[test]
fn details_follow_the_selection() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    assert!(render(&mut app, t0, 120, 20).contains("/r/proj"));
    app.key(press(KeyCode::Down), t0);
    assert!(stale(&app));
    read_extras(&m, &mut app);
    let out = render(&mut app, t0, 120, 20);
    assert!(
        out.contains("╭ notes.txt ") && out.contains("/r/notes.txt"),
        "{out}"
    );
    assert!(out.contains("│ file "), "{out}");
    assert!(!out.contains("largest by"), "{out}");
    // Closed, the pane needs no reads; opened again, it does.
    app.key(press(KeyCode::Char('i')), t0);
    assert!(!stale(&app));
    app.key(press(KeyCode::Char('i')), t0);
    assert!(stale(&app));
}

#[test]
fn tab_focuses_and_scrolls_the_pane() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let first = render(&mut app, t0, 80, 24);
    assert!(
        first.contains("↓ more") && first.contains("│ /r/proj"),
        "{first}"
    );
    app.key(press(KeyCode::Tab), t0);
    assert!(focused(&app));
    let selected = app.selected_index();
    app.key(press(KeyCode::Down), t0);
    assert_eq!(app.selected_index(), selected, "the list did not move");
    let out = render(&mut app, t0, 80, 24);
    assert!(!out.contains("│ /r/proj"), "scrolled past the path:\n{out}");
    for _ in 0..10 {
        app.key(press(KeyCode::PageDown), t0);
    }
    let end = render(&mut app, t0, 80, 24);
    assert!(end.contains("inside: 1 mount"), "{end}");
    assert!(!end.contains("↓ more"), "{end}");
    app.key(press(KeyCode::PageUp), t0);
    app.key(press(KeyCode::Esc), t0);
    assert!(!focused(&app));
    app.key(press(KeyCode::Down), t0);
    assert_ne!(
        app.selected_index(),
        selected,
        "the list has the keys again"
    );
    app.key(press(KeyCode::Tab), t0);
    app.key(press(KeyCode::Char('i')), t0);
    assert!(!app.details.open && !focused(&app));
}

#[test]
fn the_wheel_scrolls_the_pane_without_focus() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    render(&mut app, t0, 80, 24);
    assert!(scroll(&mut app, 3));
    assert!(!focused(&app));
    let out = render(&mut app, t0, 80, 24);
    assert!(!out.contains("│ /r/proj"), "{out}");
    assert!(scroll(&mut app, -3));
    assert!(render(&mut app, t0, 80, 24).contains("│ /r/proj"));
    toggle(&mut app);
    assert!(!scroll(&mut app, 1));
}

#[test]
fn focus_is_visible_without_color() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let unfocused = render(&mut app, t0, 120, 20);
    assert!(unfocused.contains("╭ proj/"), "{unfocused}");
    app.key(press(KeyCode::Tab), t0);
    let focused = render(&mut app, t0, 120, 20);
    assert!(focused.contains("┏ proj/"), "{focused}");
    assert!(focused.contains('┃') && !focused.contains('╭'), "{focused}");
}

#[test]
fn largest_children_skipped_for_huge_directories() {
    let m = model();
    let mut app = app_on(&m);
    let t0 = app.started;
    let info = read_within::<9>(&m, &app);
    let names: Vec<String> = match info.facts.as_ref().map(|f| &f.largest) {
        Some(Largest::Listed(kids)) => kids.iter().map(|k| k.name.clone()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(names, ["big.iso", "a.o", "b.o", "c.txt", "d.txt"]);
    let info = read_within::<8>(&m, &app);
    assert!(matches!(
        info.facts.as_ref().map(|f| &f.largest),
        Some(Largest::Skipped(9))
    ));
    app.extras.details = info;
    let out = render(&mut app, t0, 150, 30);
    assert!(
        out.contains("largest children: not computed for 9 entries"),
        "{out}"
    );
    assert!(!out.contains("big.iso"), "{out}");
}

#[test]
fn dates_are_utc() {
    assert_eq!(date(0), "1970-01-01 00:00 UTC");
    assert_eq!(date(MTIME), "2026-10-24 09:42 UTC");
    assert_eq!(date(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(date(-1), "1969-12-31 23:59 UTC");
}

#[test]
fn long_text_wraps_at_spaces_and_cuts_long_words() {
    assert_eq!(wrap("ab cd ef", 5), ["ab cd", "ef"]);
    assert_eq!(wrap("/very/long/path", 6), ["/very/", "long/p", "ath"]);
    assert_eq!(wrap("", 4), [""]);
}
