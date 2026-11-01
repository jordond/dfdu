use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_engine::Model;
use dfdu_engine::test_support::{add_dir, add_file};
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::{DirState, HostPath, Ix, Totals};
use ratatui::crossterm::event::KeyCode;
use ratatui::style::Color;

use super::*;
use crate::app::{App, Effect};
use crate::format::human;
use crate::model::snapshot::{self, EntryData, Mark, Snapshot, test_entry};
use crate::tests::{
    press, render, render_buffer, root_model, select, snap_without_totals, style_at, styled, text,
};
use crate::theme::Theme;

/// Reads the model as the run loop does for a frame with a new snapshot.
fn sync(app: &mut App, m: &Model, now: Instant) {
    let mut target = app.session.target.clone();
    let snap = snapshot::build(m, &mut target, app.session.size_mode);
    let marked = dfdu_session::marks::read(m, &app.session);
    app.session.target = target;
    app.refresh(snap, now);
    dfdu_session::marks::apply(&mut app.session, &marked);
}

/// The line of `frame` that shows `name`.
#[track_caller]
fn line<'a>(frame: &'a str, name: &str) -> &'a str {
    frame
        .lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("{name} missing:\n{frame}"))
}

fn complete(m: &mut Model, dir: Ix) {
    m.tree.dir_mut(dir).expect("directory").state = DirState::Complete;
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).expect("temp dir"))
        .expect("tempdir")
}

fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    fs::write(p, vec![7u8; bytes]).expect("write");
}

fn engine(journal: Option<&Path>, read_only: bool) -> Engine<PortableBackend> {
    Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: journal.map(Path::to_path_buf),
            read_only,
            ..Default::default()
        },
    )
}

fn scan(e: &Engine<PortableBackend>, root: &Path) {
    e.send(Command::Scan {
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
}

/// Confirms a permanent delete of what the dialog targets and lets the engine carry it out.
fn delete_permanently(app: &mut App, e: &Engine<PortableBackend>, t: Instant) {
    app.key(press(KeyCode::Char('D')), t);
    for c in "delete".chars() {
        app.key(press(KeyCode::Char(c)), t);
    }
    for fx in app.key(press(KeyCode::Enter), t) {
        let Effect::Send(cmd) = fx else {
            panic!("expected a command");
        };
        e.send(cmd);
    }
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
}

#[test]
fn space_marks_and_moves_down() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let es = vec![
        test_entry(0, "a", 3000),
        test_entry(1, "b", 2000),
        test_entry(2, "c", 1000),
    ];
    app.refresh(snap_without_totals(es.clone(), 1), t0);
    assert_eq!(summary(&app), None);
    assert!(!dfdu_session::marks::stale(&app.session));
    assert!(app.key(press(KeyCode::Char(' ')), t0).is_empty());
    assert!(
        dfdu_session::marks::stale(&app.session),
        "a new mark needs a read"
    );
    assert_eq!(app.session.selected_index(), Some(1));
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.session.selected_index(), Some(2));
    assert_eq!(summary(&app), Some(format!("● 2 marked ({})", human(5000))));
    assert!(is_marked(&app, &es[0]));
    assert!(!is_marked(&app, &es[2]));
    // On the last row the cursor stays; a second Space unmarks.
    app.key(press(KeyCode::Char(' ')), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(app.session.selected_index(), Some(2));
    app.key(press(KeyCode::Char('g')), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    assert_eq!(summary(&app), Some(format!("● 1 marked ({})", human(2000))));
    assert!(!is_marked(&app, &es[0]));
    assert!(is_marked(&app, &es[1]));
    clear(&mut app);
    assert_eq!(summary(&app), None);
    assert!(!is_marked(&app, &es[1]));
}

#[test]
fn space_on_shared_row_is_refused() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let shared = EntryData {
        pseudo: true,
        ..test_entry(1, "(shared between entries)", 5000)
    };
    let gone = test_entry(2, "gone", 4000);
    let a = test_entry(0, "a", 10);
    app.refresh(
        snap_without_totals(vec![shared.clone(), gone.clone(), a.clone()], 1),
        t0,
    );
    // A deleted entry stays in its row until the next re-sort.
    let ghost = EntryData {
        pseudo: true,
        ghost: true,
        ..gone
    };
    app.refresh(snap_without_totals(vec![shared.clone(), ghost, a], 2), t0);
    assert!(app.session.snap.entries[app.session.list.rows[1]].ghost);
    for row in 0..2 {
        app.session.move_to(row);
        app.key(press(KeyCode::Char(' ')), t0);
        assert!(app.session.marks.is_empty());
        assert_eq!(app.session.selected_index(), Some(row), "the cursor stays");
        assert_eq!(
            app.session.status.as_deref(),
            Some("only entries can be marked")
        );
    }
    assert!(!dfdu_session::marks::stale(&app.session));
    // The shared row has the directory's reference; marking the directory from its parent
    // must not put a mark on it.
    assert!(!is_marked(&app, &shared));
}

#[test]
fn marks_survive_navigation_and_rescan_swap() {
    let tmp = tempdir();
    let root = tmp.path();
    write(&root.join("sub/inner/f"), 4000);
    write(&root.join("sub/other.bin"), 1000);
    let e = engine(None, false);
    scan(&e, root);
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    e.read(|m| sync(&mut app, m, t));
    select(&mut app, "sub");
    app.key(press(KeyCode::Enter), t);
    e.read(|m| sync(&mut app, m, t));
    select(&mut app, "inner");
    app.key(press(KeyCode::Char(' ')), t);
    let old = app.session.marks.items()[0].target.node;
    app.key(press(KeyCode::Left), t);
    e.read(|m| sync(&mut app, m, t));
    assert!(!dfdu_session::marks::stale(&app.session));
    let out = render(&mut app, t, 90, 8);
    assert!(line(&out, "sub/").starts_with("  "), "{out}");
    assert!(line(&out, "1 marked").contains("● 1 marked"), "{out}");

    select(&mut app, "sub");
    app.key(press(KeyCode::Enter), t);
    e.read(|m| sync(&mut app, m, t));
    let out = render(&mut app, t, 90, 8);
    assert!(line(&out, "inner/").starts_with('●'), "{out}");
    assert!(line(&out, "other.bin").starts_with("  "), "{out}");

    // Rescanning sub replaces its children with new nodes.
    e.send(Command::Rescan {
        node: app.session.snap.dir_node.expect("sub"),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    e.read(|m| sync(&mut app, m, t));
    let inner = app
        .session
        .snap
        .entries
        .iter()
        .find(|en| en.name == b"inner")
        .expect("inner")
        .node;
    assert_ne!(Some(inner), old, "the rescan swapped the node");
    assert_eq!(app.session.marks.items().len(), 1);
    assert_eq!(app.session.marks.items()[0].target.node, Some(inner));
    let out = render(&mut app, t, 90, 8);
    assert!(line(&out, "inner/").starts_with('●'), "{out}");
    e.shutdown(Duration::from_millis(200));
}

#[test]
fn nested_marks_count_once() {
    let (mut m, root) = root_model();
    let a = add_dir(&mut m.tree, root, "a", 2);
    add_file(&mut m.tree, a, "b", 6000, 3);
    add_file(&mut m.tree, a, "c", 2000, 4);
    add_file(&mut m.tree, root, "d", 1000, 5);
    for d in [a, root] {
        complete(&mut m, d);
    }
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    sync(&mut app, &m, t);
    select(&mut app, "a");
    app.key(press(KeyCode::Char(' ')), t);
    app.key(press(KeyCode::Up), t);
    app.key(press(KeyCode::Enter), t);
    sync(&mut app, &m, t);
    select(&mut app, "b");
    app.key(press(KeyCode::Char(' ')), t);
    app.key(press(KeyCode::Left), t);
    sync(&mut app, &m, t);
    let a_total = m.tree.total(a).alloc;
    assert_eq!(app.session.marks.total(), (a_total, false));
    assert_eq!(
        summary(&app),
        Some(format!("● 2 marked ({})", human(a_total)))
    );
    // b marked first, inside a marked after it: still counted once.
    let mut app2 = App::new(Theme::no_color(), t);
    sync(&mut app2, &m, t);
    select(&mut app2, "a");
    app2.key(press(KeyCode::Enter), t);
    sync(&mut app2, &m, t);
    select(&mut app2, "b");
    app2.key(press(KeyCode::Char(' ')), t);
    app2.key(press(KeyCode::Left), t);
    sync(&mut app2, &m, t);
    select(&mut app2, "a");
    app2.key(press(KeyCode::Char(' ')), t);
    sync(&mut app2, &m, t);
    assert_eq!(app2.session.marks.total(), (a_total, false));

    app.key(press(KeyCode::Char('d')), t);
    let out = render(&mut app, t, 100, 20);
    assert!(out.contains("Move a/ to the Trash?"), "{out}");
    assert!(
        line(&out, "a/b").contains("inside another marked item"),
        "{out}"
    );
    let fx = app.key(press(KeyCode::Char('y')), t);
    let a_ref = m.tree.node_ref(a);
    assert!(
        matches!(fx.as_slice(), [Effect::Send(Command::Delete { items, .. })] if items == &[a_ref]),
        "{fx:?}"
    );
}

#[test]
fn totals_follow_live_sizes() {
    let (mut m, root) = root_model();
    let big = add_dir(&mut m.tree, root, "big", 2);
    // Still being scanned.
    m.tree.dir_mut(big).expect("big").state = DirState::Pending;
    add_file(&mut m.tree, big, "f1", 100_000, 3);
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    sync(&mut app, &m, t);
    select(&mut app, "big");
    app.key(press(KeyCode::Char(' ')), t);
    assert!(dfdu_session::marks::stale(&app.session));
    sync(&mut app, &m, t);
    assert!(
        !dfdu_session::marks::stale(&app.session),
        "a read takes the new mark in"
    );
    let size = |m: &Model| human(m.tree.total(big).alloc);
    let first = summary(&app);
    assert_eq!(first, Some(format!("● 1 marked (at least {})", size(&m))));
    add_file(&mut m.tree, big, "f2", 2_000_000, 4);
    sync(&mut app, &m, t);
    assert_eq!(
        summary(&app),
        Some(format!("● 1 marked (at least {})", size(&m)))
    );
    assert_ne!(summary(&app), first, "the total grew with the scan");
    complete(&mut m, big);
    sync(&mut app, &m, t);
    assert_eq!(summary(&app), Some(format!("● 1 marked ({})", size(&m))));
    // The size mode of the read decides the size.
    app.session.size_mode = dfdu_engine::SizeMode::Apparent;
    sync(&mut app, &m, t);
    assert_eq!(app.session.marks.total().0, m.tree.total(big).apparent);
}

#[test]
fn deleted_marks_are_dropped() {
    let tmp = tempdir();
    let root = tmp.path();
    write(&root.join("junk/b.bin"), 50_000);
    write(&root.join("file.bin"), 3_000);
    write(&root.join("keep.bin"), 1_000);
    let journal = tempdir();
    let e = engine(Some(journal.path()), false);
    scan(&e, root);
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    e.read(|m| sync(&mut app, m, t));
    for name in ["junk", "file.bin"] {
        select(&mut app, name);
        app.key(press(KeyCode::Char(' ')), t);
    }
    e.read(|m| sync(&mut app, m, t));
    assert_eq!(app.session.marks.items().len(), 2);
    // file.bin changes after the scan: the delete leaves it alone.
    write(&root.join("file.bin"), 4_000);
    delete_permanently(&mut app, &e, t);
    e.read(|m| sync(&mut app, m, t));
    e.shutdown(Duration::from_millis(200));
    assert!(!root.join("junk").exists());
    let status = app.session.status.clone().unwrap_or_default();
    assert!(status.contains("deleted 1"), "{status}");
    assert!(status.contains("1 changed since the scan"), "{status}");
    let left: Vec<_> = app
        .session
        .marks
        .items()
        .iter()
        .map(|it| it.target.path.clone())
        .collect();
    assert_eq!(left, [HostPath::local(&root.join("file.bin"))]);
    let out = render(&mut app, t, 90, 8);
    assert!(line(&out, "file.bin").starts_with('●'), "{out}");
    assert!(line(&out, "keep.bin").starts_with("  "), "{out}");
}

#[test]
fn refused_delete_keeps_the_marks() {
    let tmp = tempdir();
    let root = tmp.path();
    write(&root.join("junk/b.bin"), 50_000);
    let journal = tempdir();
    let e = engine(Some(journal.path()), true);
    scan(&e, root);
    let t = Instant::now();
    let mut app = App::new(Theme::no_color(), t);
    e.read(|m| sync(&mut app, m, t));
    select(&mut app, "junk");
    app.key(press(KeyCode::Char(' ')), t);
    delete_permanently(&mut app, &e, t);
    e.read(|m| sync(&mut app, m, t));
    e.shutdown(Duration::from_millis(200));
    assert!(root.join("junk").exists());
    assert_eq!(app.session.marks.items().len(), 1);
    let out = render(&mut app, t, 90, 8);
    assert!(line(&out, "junk/").starts_with('●'), "{out}");
    assert_eq!(
        app.session.snap.notice.as_deref(),
        Some("delete: read-only mode")
    );
}

/// A scan 14 s in with two marked entries, one of them still being scanned.
fn marked_view(theme: Theme) -> ratatui::buffer::Buffer {
    let t0 = Instant::now();
    let mut app = App::new(theme, t0);
    let gib = 1 << 30;
    let dir = |i, name: &str, size, mark| EntryData {
        is_dir: true,
        mark,
        items: 1000,
        ..test_entry(i, name, size)
    };
    let entries = vec![
        dir(0, "Library", 112 * gib, Mark::Scanning),
        dir(1, "dev", 78 * gib, Mark::None),
        dir(2, ".android", 51 * gib, Mark::None),
        test_entry(3, "notes.sqlite", 625 << 20),
    ];
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let totals = Totals {
        apparent: total,
        alloc: total,
        items: 3004,
        ..Totals::default()
    };
    app.refresh(
        Snapshot {
            dir_path: "/home/jo".into(),
            dir_node: Some(test_entry(99, "/home/jo", 0).node),
            at_root: true,
            dir_total: totals,
            dir_mark: Some(Mark::Scanning),
            root_total: totals,
            entries,
            busy: true,
            version: 1,
            ..Default::default()
        },
        t0,
    );
    app.key(press(KeyCode::Char(' ')), t0);
    app.key(press(KeyCode::Down), t0);
    app.key(press(KeyCode::Char(' ')), t0);
    render_buffer(&mut app, t0 + Duration::from_secs(14), 100, 10)
}

#[test]
fn marked_rows_show_a_glyph_in_mono() {
    let out = text(&marked_view(Theme::no_color()));
    insta::assert_snapshot!("marks_mono", out);
    assert!(line(&out, "Library/").starts_with('●'), "{out}");
    assert!(line(&out, ".android/").starts_with('●'), "{out}");
    assert!(line(&out, "dev/").starts_with("  "), "{out}");
    assert!(out.contains("● 2 marked (at least 163.0 GiB)"), "{out}");

    let buf = marked_view(Theme::ansi16());
    insta::assert_snapshot!("marks_ansi16", styled(&buf));
    let mark = Theme::ansi16().mark;
    assert_eq!(style_at(&buf, "● 2 marked").fg, mark.fg);
    assert_ne!(mark.fg, None);
    assert_ne!(mark.fg, Some(Color::Reset));
}
