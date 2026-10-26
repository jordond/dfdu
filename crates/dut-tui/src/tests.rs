//! Frame tests with ratatui's `TestBackend`: a hand-built mid-scan view, and a view of a real
//! scan of a temporary directory.

use std::fs;
use std::time::{Duration, Instant};

use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{JobOptions, PortableBackend};
use dut_tree::Totals;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::app::App;
use crate::snapshot::{self, EntryData, Mark, Snapshot, Target, test_entry};
use crate::theme::Theme;
use crate::view;

fn text(buf: &Buffer) -> String {
    let w = buf.area.width as usize;
    let mut out = String::new();
    for (i, cell) in buf.content.iter().enumerate() {
        out.push_str(cell.symbol());
        if (i + 1) % w == 0 {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
    }
    out
}

pub(crate) fn render(app: &mut App, now: Instant, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    term.draw(|f| view::draw(f, app, now)).expect("draw");
    text(term.backend().buffer())
}

fn dir(i: u32, name: &str, size: u64, items: u64, mark: Mark) -> EntryData {
    EntryData {
        is_dir: true,
        items,
        mark,
        ..test_entry(i, name, size)
    }
}

#[test]
fn mid_scan_view() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let root = test_entry(99, "/home/jo", 0);
    let gib = 1 << 30;
    let entries = vec![
        dir(0, "Library", 112 * gib, 812_000, Mark::Scanning),
        dir(1, "dev", 78 * gib, 1_100_000, Mark::Scanning),
        dir(2, ".android", 51 * gib, 214, Mark::None),
        dir(3, "nfs", 0, 1, Mark::Stalled),
        EntryData {
            problem: Some("permission denied".into()),
            ..dir(4, "locked", 0, 1, Mark::Error)
        },
        test_entry(5, "notes.sqlite", 625 << 20),
    ];
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let snap = Snapshot {
        dir_path: std::path::PathBuf::from("/home/jo"),
        dir_node: Some(root.node),
        at_root: true,
        dir_total: Totals {
            apparent: total,
            alloc: total,
            items: 1_912_217,
            errors: 1,
            ..Totals::default()
        },
        dir_mark: Some(Mark::Scanning),
        root_total: Totals {
            apparent: total,
            alloc: total,
            items: 1_912_217,
            errors: 1,
            ..Totals::default()
        },
        entries,
        busy: true,
        stalled: 1,
        version: 1,
        ..Default::default()
    };
    app.refresh(snap, t0);
    let out = render(&mut app, t0 + Duration::from_secs(14), 100, 12);
    insta_like(&out, "mid_scan");
}

/// Compares against a stored frame in `tests/frames/<name>.txt`; writes it if missing or if
/// `DUT_UPDATE_FRAMES` is set, so a reviewer sees changes in the diff.
fn insta_like(out: &str, name: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/frames")
        .join(format!("{name}.txt"));
    if std::env::var_os("DUT_UPDATE_FRAMES").is_some() || !path.exists() {
        fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        fs::write(&path, out).expect("write frame");
        return;
    }
    // A checkout with CRLF conversion (Windows) must not count as a changed frame.
    let want = fs::read_to_string(&path)
        .expect("read frame")
        .replace("\r\n", "\n");
    assert_eq!(
        out, want,
        "frame {name} changed; rerun with DUT_UPDATE_FRAMES=1 to accept"
    );
}

/// A temporary directory under the canonical temp dir: the engine canonicalises scan roots (on
/// macOS the temp dir is behind the /var symlink), and paths are compared with the tree's.
fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).expect("temp dir"))
        .expect("tempdir")
}

#[test]
fn completed_scan_view() {
    let tmp = tempdir();
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("big")).expect("mkdir");
    fs::create_dir_all(dir.join("small")).expect("mkdir");
    fs::write(dir.join("big/blob"), vec![7u8; 400_000]).expect("write");
    fs::write(dir.join("small/a"), vec![1u8; 10_000]).expect("write");
    fs::write(dir.join("top.txt"), vec![2u8; 50_000]).expect("write");

    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: dir.clone(),
        opts: JobOptions::default(),
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))));
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.size_mode = crate::SizeMode::Apparent;
    let mut target = Target::root();
    let snap = engine.read(|m| snapshot::build(m, &mut target, crate::SizeMode::Apparent));
    app.refresh(snap, t0 + Duration::from_secs(1));
    let out = render(&mut app, t0 + Duration::from_secs(1), 90, 10);
    engine.shutdown(Duration::from_millis(200));

    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("done in"), "{out}");
    assert!(!out.contains('⠋'), "no spinner after completion:\n{out}");
    let pos = |name: &str| {
        lines
            .iter()
            .position(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name} missing:\n{out}"))
    };
    // Largest first: big/ (400 kB), top.txt (50 kB), small/ (10 kB).
    assert!(
        pos("big/") < pos("top.txt") && pos("top.txt") < pos("small/"),
        "{out}"
    );
    // The cursor starts on the first row.
    assert_eq!(
        app.selected_entry().map(|e| e.name.to_vec()),
        Some(b"big".to_vec())
    );
}

#[test]
fn target_survives_rescan_swap() {
    let tmp = tempdir();
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("sub/inner")).expect("mkdir");
    fs::write(dir.join("sub/inner/f"), vec![0u8; 1000]).expect("write");

    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: dir.clone(),
        opts: JobOptions::default(),
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))));
    let sub_path = dir.join("sub");
    let mut target = engine.read(|m| {
        let sub = snapshot::find_path(&m.tree, &sub_path).expect("sub");
        Target::of(&m.tree, sub)
    });
    let old = target.node.expect("node");
    engine.send(Command::Rescan { node: old });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))));
    let snap = engine.read(|m| snapshot::build(m, &mut target, crate::SizeMode::Disk));
    engine.shutdown(Duration::from_millis(200));

    assert_ne!(target.node, Some(old), "the rescan replaced the node");
    assert!(
        snap.dir_path.ends_with("sub"),
        "still in sub: {:?}",
        snap.dir_path.display()
    );
    assert_eq!(snap.entries.len(), 1);
    assert_eq!(snap.entries[0].name, b"inner");
}

#[test]
fn overlays_fit_a_small_terminal() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let node = test_entry(1, "junk", 0).node;
    let list = crate::ErrorList {
        root: "/r".into(),
        items: (0..50)
            .map(|i| crate::ErrorItem {
                node,
                path: format!("/r/some/rather/long/path/number/{i}").into(),
                is_dir: true,
                problem: "permission denied".into(),
            })
            .collect(),
        total: 60,
        kinds: vec![(dut_tree::ErrKind::PermissionDenied, 60)],
    };
    for overlay in [
        crate::app::Overlay::Help,
        crate::app::Overlay::Errors { list, cursor: 49 },
        crate::app::Overlay::ConfirmDelete {
            node,
            name: "junk".into(),
            size: 5 << 20,
            shared_out: 1 << 20,
            is_dir: true,
            mode: dut_engine::Mode::Permanent,
            typed: String::new(),
            scanning: false,
        },
    ] {
        app.overlay = overlay;
        // Smaller than either dialog: the dialog is clamped to the screen.
        let out = render(&mut app, t0, 40, 8);
        assert_eq!(out.lines().count(), 8, "{out}");
        let out = render(&mut app, t0, 100, 30);
        assert!(out.contains('┌'), "{out}");
        if matches!(app.overlay, crate::app::Overlay::Errors { .. }) {
            // Scrolled to the cursor, with the count of what is not listed.
            assert!(out.contains("number/49/  permission denied"), "{out}");
            assert!(out.contains("first 50 shown"), "{out}");
        }
    }
}

#[test]
fn shared_data_has_its_own_row_and_cannot_be_deleted() {
    use dut_tree::{Kind, Meta};
    let meta = |kind, size, ino| Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    };
    let mut m = dut_engine::Model::default();
    let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    for (i, name) in [b"a".as_slice(), b"b"].iter().enumerate() {
        let f = m.tree.add_file(
            root,
            name,
            &meta(Kind::File, 1000, 10 + i as u64),
            false,
            None,
        );
        let own = m.tree.own(f);
        m.tree.propagate_add(root, &own);
        // Each file holds 400 bytes alone; 600 they share are counted at the directory.
        m.tree.set_measured(f, 400, false);
    }
    m.tree.add_home(root, 600, false);
    let mut target = Target::root();
    let snap = snapshot::build(&m, &mut target, crate::SizeMode::Disk);
    let shared: Vec<_> = snap.entries.iter().filter(|e| e.pseudo).collect();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].size, 600);
    assert_eq!(snap.dir_total.alloc, 1400);
    let like_du = snapshot::build(&m, &mut target, crate::SizeMode::Referenced);
    assert!(like_du.entries.iter().all(|e| !e.pseudo));
    assert_eq!(like_du.dir_total.referenced, 2000);

    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(snap, t0);
    let out = render(&mut app, t0, 90, 8);
    assert!(out.contains("(shared between entries)"), "{out}");
    let at = app
        .rows
        .iter()
        .position(|&i| app.snap.entries[i].pseudo)
        .unwrap();
    for _ in 0..at {
        app.key(key(ratatui::crossterm::event::KeyCode::Down), t0);
    }
    app.key(key(ratatui::crossterm::event::KeyCode::Char('D')), t0);
    assert!(matches!(app.overlay, crate::app::Overlay::None));
}

pub(crate) fn key(code: ratatui::crossterm::event::KeyCode) -> ratatui::crossterm::event::KeyEvent {
    ratatui::crossterm::event::KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE)
}

#[test]
fn errors_are_listed_and_lead_to_the_entry() {
    use dut_tree::{ErrKind, Kind, Meta};
    use ratatui::crossterm::event::KeyCode;
    let meta = |kind, ino| Meta {
        kind,
        apparent: 100,
        alloc: 100,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    };
    let mut m = dut_engine::Model::default();
    let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 1), true);
    let a = m.tree.add_dir_flagged(root, b"a", &meta(Kind::Dir, 2), 0);
    let b = m.tree.add_dir_flagged(a, b"b", &meta(Kind::Dir, 3), 0);
    let locked = m.tree.add_dir_flagged(b, b"locked", &meta(Kind::Dir, 4), 0);
    let gone = m
        .tree
        .add_file(root, b"gone", &meta(Kind::File, 5), false, None);
    for d in [root, a, b, locked] {
        m.tree.dir_mut(d).unwrap().state = dut_tree::DirState::Complete;
    }
    m.tree.dir_mut(locked).unwrap().state = dut_tree::DirState::Failed;
    m.tree.set_error(locked, ErrKind::PermissionDenied);
    m.tree.set_error(gone, ErrKind::NotFound);

    let list = crate::errors(&m.tree, root, 100);
    assert_eq!(list.total, 2);
    let paths: Vec<_> = list.items.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths,
        [
            std::path::Path::new("/r/a/b/locked"),
            std::path::Path::new("/r/gone")
        ]
    );
    assert_eq!(
        list.summary(),
        "1 permission denied, 1 removed during the scan"
    );

    let mut target = Target::root();
    let snap = snapshot::build(&m, &mut target, crate::SizeMode::Disk);
    let entry = |name: &[u8]| {
        snap.entries
            .iter()
            .find(|e| e.name[..] == name[..])
            .unwrap()
    };
    assert_eq!(entry(b"a").mark, Mark::ErrorsInside);
    assert_eq!(entry(b"a").errors_inside, 1);
    assert_eq!(
        entry(b"gone").problem.as_deref(),
        Some("removed during the scan")
    );

    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    app.refresh(snap, t0);
    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("1 error inside"), "{out}");
    assert!(out.contains("! errors"), "{out}");
    let fx = app.key(key(KeyCode::Char('!')), t0);
    assert!(matches!(fx[..], [crate::Effect::ShowErrors]));
    app.show_errors(list);
    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("2 not read"), "{out}");
    // Paths are shown with the platform's separator.
    let shown = std::path::Path::new("a").join("b").join("locked");
    assert!(
        out.contains(&format!("{}/  permission denied", shown.display())),
        "{out}"
    );
    app.key(key(KeyCode::Enter), t0);
    assert!(matches!(app.overlay, crate::app::Overlay::None));
    let mut target = app.target.clone();
    let snap = snapshot::build(&m, &mut target, crate::SizeMode::Disk);
    app.target = target;
    app.refresh(snap, t0);
    assert_eq!(app.snap.dir_path, std::path::Path::new("/r/a/b"));
    let sel = app.selected_entry().expect("selected");
    assert_eq!(sel.name, b"locked");
    assert_eq!(sel.problem.as_deref(), Some("permission denied"));
}

#[test]
fn disk_picker() {
    use dut_sys::{Disk, FsClass, Space};
    use ratatui::crossterm::event::KeyCode;
    let gib = 1u64 << 30;
    let disk = |mp: &str, fstype: &str, class, space| Disk {
        mount_point: mp.into(),
        also_at: vec![],
        fstype: fstype.into(),
        source: String::new(),
        class,
        space,
    };
    let mut p = crate::Picker::new(vec![
        Disk {
            also_at: vec!["/home".into()],
            ..disk(
                "/",
                "btrfs",
                FsClass::Local,
                Some(Space {
                    total: 500 * gib,
                    used: 400 * gib,
                    available: 90 * gib,
                }),
            )
        },
        disk("/mnt/nas", "nfs4", FsClass::Network, None),
        disk(
            "/mnt/usb",
            "exfat",
            FsClass::Local,
            Some(Space {
                total: 64 * gib,
                used: 0,
                available: 64 * gib,
            }),
        ),
    ]);
    let theme = Theme::no_color();
    let mut term = Terminal::new(TestBackend::new(110, 8)).expect("terminal");
    term.draw(|f| crate::picker::draw(f, &p, &theme))
        .expect("draw");
    let out = text(term.backend().buffer());
    insta_like(&out, "disk_picker");
    assert!(out.contains("also at /home"), "{out}");
    assert!(out.contains("not responding  network"), "{out}");

    // A mount that does not answer is not scanned; the next one is.
    assert!(p.key(KeyCode::Down).is_none());
    assert!(p.key(KeyCode::Enter).is_none());
    assert!(p.status.as_deref().unwrap().contains("not responding"));
    assert!(p.key(KeyCode::Down).is_none());
    assert!(p.key(KeyCode::Down).is_none());
    match p.key(KeyCode::Enter) {
        Some(crate::Picked::Scan(path)) => assert_eq!(path, std::path::Path::new("/mnt/usb")),
        _ => panic!("expected a scan of /mnt/usb"),
    }
    assert!(matches!(
        p.key(KeyCode::Char('q')),
        Some(crate::Picked::Quit)
    ));
}
