use std::fs;
use std::time::{Duration, Instant};

use dfdu_engine::{Command as EngineCommand, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::model::snapshot;
use crate::tests::{press, render, render_buffer};
use crate::theme::Theme;

#[test]
fn duplicates_are_found_marked_and_shown() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("photos/old")).unwrap();
    let data: Vec<u8> = (0..(u32::try_from(min_size()).unwrap() + 5000))
        .map(|i| (i % 249) as u8)
        .collect();
    fs::write(dir.join("photos/a.jpg"), &data).unwrap();
    fs::write(dir.join("photos/old/a copy.jpg"), &data).unwrap();
    fs::write(dir.join("b.jpg"), &data).unwrap();
    fs::write(dir.join("other.jpg"), vec![1u8; data.len()]).unwrap();
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            ..Default::default()
        },
    );
    engine.send(EngineCommand::Scan {
        path: dir.clone().into(),
        opts: JobOptions::default(),
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut target = Target::root();
    let snap = engine.read(|m| snapshot::build(m, &mut target, app.size_mode));
    app.target = target;
    app.refresh(snap, t0);

    // Another machine's tree (remote or from a file) is not compared with this one's files.
    assert!(app.key(press(KeyCode::Char('4')), t0).is_empty());
    assert!(
        app.status
            .as_deref()
            .unwrap()
            .contains("this machine's files")
    );
    app.outside.local = true;
    let fx = app.key(press(KeyCode::Char('4')), t0);
    assert!(matches!(fx[..], [Effect::FindDupes]));
    let (root, cands) = engine.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (
            m.tree.path_buf(r),
            dfdu_dupes::candidates(&m.tree, r, min_size()),
        )
    });
    assert_eq!(cands.len(), 4);
    begin(&mut app, root, cands);
    let deadline = Instant::now() + Duration::from_secs(20);
    while running(&app) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(stale(&app), "the files need their nodes");
    app.extras.dupes = engine.read(|m| read(m, &app));
    assert!(!stale(&app));
    engine.shutdown(Duration::from_millis(200));

    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("3 copies of 1.0 MiB"), "{out}");
    assert!(out.contains("photos/old/a copy.jpg"), "{out}");
    assert!(!out.contains("other.jpg"), "{out}");
    // `a` marks every copy but the oldest.
    app.key(press(KeyCode::Char('a')), t0);
    assert_eq!(app.marks.items().len(), 2);
    app.key(press(KeyCode::Char('a')), t0);
    assert_eq!(app.marks.items().len(), 2);
    // Space on the group unmarks its copies again.
    app.dupes.cursor = 0;
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(app.marks.is_empty());
    // A deleted file leaves the group; two left still make one.
    let gone = found(&app).unwrap().groups[0].files[0].path.clone();
    app.extras.dupes.insert(gone, None);
    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("2 copies of 1.0 MiB"), "{out}");
}

/// A comparison done with `groups` found: files below `/r` with their dates, each still in the
/// tree.
fn found_in(app: &mut App, groups: &[&[(&str, i64)]]) {
    let mut bits = 0;
    let mut file = |path: &str, mtime: i64| {
        bits += 1;
        app.extras
            .dupes
            .insert(path.into(), Some(NodeRef::from_bits(bits)));
        Candidate {
            path: path.into(),
            size: 1 << 20,
            alloc: 1 << 20,
            dev_ino: (1, bits),
            mtime,
            links: 0,
            private: None,
        }
    };
    let groups = (groups.iter())
        .map(|files| Group {
            size: 1 << 20,
            hash: [0; 32],
            files: files.iter().map(|&(p, mtime)| file(p, mtime)).collect(),
        })
        .collect();
    let result = Arc::new(OnceLock::new());
    result
        .set(Found {
            groups,
            compared: 4,
            ..Found::default()
        })
        .unwrap();
    app.dupes = Dupes {
        on: true,
        run: Some(Run {
            root: "/r".into(),
            progress: Arc::default(),
            cancel: Arc::default(),
            result,
        }),
        ..Dupes::default()
    };
}

/// The directory a group's files share goes in its header; each row shows the rest of its path,
/// shortened in the middle to leave the dates a column at the right edge.
#[test]
fn rows_show_the_path_below_the_shared_directory_and_align_the_dates() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let trip = "/r/photos/2026/trip-to-the-coast";
    let long = format!("{trip}/edited/exports for the family album/IMG_0001 (final, small).jpg");
    found_in(
        &mut app,
        &[
            &[
                (&format!("{trip}/IMG_0001.jpg"), 1_760_000_000),
                (&long, 1_761_000_000),
            ],
            &[
                ("/r/a.bin", 1_700_000_000),
                ("/r/backup/a.bin", 1_700_000_000),
            ],
        ],
    );
    // `render` shows `\` as `/`, so the frame is the same on every platform.
    let out = render(&mut app, t0, 80, 12);
    insta::assert_snapshot!("dupes_80x12", out);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[3].contains("· in photos/2026/trip-to-the-coast/"),
        "{out}"
    );
    // The shared directory is joined with the platform's separator, as the cells hold it.
    let buf = render_buffer(&mut app, t0, 80, 12);
    let header: String = (0..80).map(|x| buf[(x, 3)].symbol()).collect();
    let sep = std::path::MAIN_SEPARATOR;
    let shared = format!("· in photos{sep}2026{sep}trip-to-the-coast{sep}");
    assert!(header.contains(&shared), "{header}");
    let files: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.ends_with("UTC"))
        .collect();
    assert_eq!(files.len(), 4, "{out}");
    assert!(files.iter().all(|l| l.chars().count() == 80), "{out}");
    assert!(files.iter().all(|l| !l.contains("photos")), "{out}");
    assert!(files[0].contains(" IMG_0001.jpg "), "{out}");
    assert!(
        files[1].contains('…') && files[1].contains("small).jpg"),
        "{out}"
    );
    // Files in the scan root share no directory: the header names none.
    assert!(!lines[6].contains(" in "), "{out}");
    assert!(files[3].contains(" backup/a.bin "), "{out}");
}

/// Marking every copy keeps the oldest, labelled `keep` in its row; `K` cycles the rule through
/// newest, shortest path and first in path order, and the summary line names it.
#[test]
fn the_keep_rule_picks_the_copy_marking_leaves() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    found_in(
        &mut app,
        &[&[
            ("/r/b/new.bin", 1_760_000_000),
            ("/r/a/archive/old.bin", 1_700_000_000),
            ("/r/c.bin", 1_730_000_000),
        ]],
    );
    let kept_after_a = |app: &mut App| {
        marks::clear(app);
        app.key(press(KeyCode::Char('a')), t0);
        let marked: Vec<String> = (app.marks.items().iter())
            .map(|i| i.target.path.display().to_string())
            .collect();
        let files = &found(app).unwrap().groups[0].files;
        let left: Vec<String> = (files.iter())
            .map(|c| c.path.display().to_string())
            .filter(|p| !marked.contains(p))
            .collect();
        assert_eq!(left.len(), 1, "{marked:?}");
        left[0].clone()
    };
    assert_eq!(kept_after_a(&mut app), "/r/a/archive/old.bin");
    let out = render(&mut app, t0, 80, 10);
    assert!(out.contains("keep: oldest"), "{out}");
    let row = |out: &str, name: &str| {
        (out.lines())
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name}: {out}"))
            .to_owned()
    };
    assert!(row(&out, "old.bin").contains("keep  "), "{out}");
    assert!(!row(&out, "new.bin").contains("keep  "), "{out}");
    insta::assert_snapshot!("dupes_keep_80x10", out);

    for want in ["newest", "shortest path", "first in path order", "oldest"] {
        app.key(press(KeyCode::Char('K')), t0);
        assert_eq!(app.dupes.keep.label(), want);
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains(want), "{status}");
        let kept = kept_after_a(&mut app);
        let out = render(&mut app, t0, 80, 10);
        assert!(out.contains(&format!("keep: {want}")), "{out}");
        let expect = match want {
            "newest" => "/r/b/new.bin",
            "shortest path" => "/r/c.bin",
            "first in path order" | "oldest" => "/r/a/archive/old.bin",
            _ => unreachable!(),
        };
        assert_eq!(kept, expect, "{want}");
        let name = expect.rsplit('/').next().unwrap();
        assert!(row(&out, name).contains("keep  "), "{out}");
    }
}

#[test]
fn half_pages_move_the_cursor_and_y_copies_the_file() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    found_in(
        &mut app,
        &[&[("/r/a", 1), ("/r/b", 2), ("/r/c", 3), ("/r/d", 4)]],
    );
    app.list_height = 4;
    let ctrl = |c| {
        KeyEvent::new(
            KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        )
    };
    app.key(ctrl('d'), t0);
    assert_eq!(app.dupes.cursor, 2);
    let fx = app.key(press(KeyCode::Char('y')), t0);
    assert!(
        matches!(&fx[..], [Effect::Copy(p)] if p.as_str() == "/r/b"),
        "{fx:?}"
    );
    app.key(ctrl('u'), t0);
    assert_eq!(app.dupes.cursor, 0);
    let fx = app.key(press(KeyCode::Char('y')), t0);
    assert!(fx.is_empty(), "a group's row has no path: {fx:?}");
}
