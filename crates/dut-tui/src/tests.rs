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

fn render(app: &mut App, now: Instant, w: u16, h: u16) -> String {
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
        dir(4, "locked", 0, 1, Mark::Error),
        test_entry(5, "notes.sqlite", 625 << 20),
    ];
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let snap = Snapshot {
        dir_path: b"/home/jo".to_vec(),
        dir_node: Some(root.node),
        at_root: true,
        dir_total: Totals {
            apparent: total,
            alloc: total,
            items: 1_912_217,
            errors: 1,
        },
        dir_mark: Some(Mark::Scanning),
        root_total: Totals {
            apparent: total,
            alloc: total,
            items: 1_912_217,
            errors: 1,
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

#[test]
fn completed_scan_view() {
    let dir = std::env::temp_dir().join(format!("dut-tui-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("big")).expect("mkdir");
    fs::create_dir_all(dir.join("small")).expect("mkdir");
    fs::write(dir.join("big/blob"), vec![7u8; 400_000]).expect("write");
    fs::write(dir.join("small/a"), vec![1u8; 10_000]).expect("write");
    fs::write(dir.join("top.txt"), vec![2u8; 50_000]).expect("write");

    let engine = Engine::start(
        PortableBackend,
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
    app.apparent = true;
    let mut target = Target::root();
    let snap = engine.read(|m| snapshot::build(m, &mut target, true));
    app.refresh(snap, t0 + Duration::from_secs(1));
    let out = render(&mut app, t0 + Duration::from_secs(1), 90, 10);
    engine.shutdown(Duration::from_millis(200));
    let _ = fs::remove_dir_all(&dir);

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
        app.selected_entry().map(|e| e.name.clone()),
        Some(b"big".to_vec())
    );
}

#[test]
fn target_survives_rescan_swap() {
    let dir = std::env::temp_dir().join(format!("dut-tui-swap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub/inner")).expect("mkdir");
    fs::write(dir.join("sub/inner/f"), vec![0u8; 1000]).expect("write");
    // The engine canonicalises scan roots (on macOS the temp dir is behind the /var symlink).
    let dir = dir.canonicalize().expect("canonicalize");

    let engine = Engine::start(
        PortableBackend,
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
        let sub =
            snapshot::find_path(&m.tree, sub_path.as_os_str().as_encoded_bytes()).expect("sub");
        Target::of(&m.tree, sub)
    });
    let old = target.node.expect("node");
    engine.send(Command::Rescan { node: old });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))));
    let snap = engine.read(|m| snapshot::build(m, &mut target, false));
    engine.shutdown(Duration::from_millis(200));
    let _ = fs::remove_dir_all(&dir);

    assert_ne!(target.node, Some(old), "the rescan replaced the node");
    assert!(
        snap.dir_path
            .ends_with(&[&[dut_tree::SEP][..], b"sub"].concat()),
        "still in sub: {:?}",
        String::from_utf8_lossy(&snap.dir_path)
    );
    assert_eq!(snap.entries.len(), 1);
    assert_eq!(snap.entries[0].name, b"inner");
}
