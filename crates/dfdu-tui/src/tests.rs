//! Frame tests with ratatui's `TestBackend`: a hand-built mid-scan view, and a view of a real
//! scan of a temporary directory. The helpers here render frames and read the styles of their
//! cells for the tests of other modules too.
//!
//! Frames are compared with insta snapshots in `src/snapshots/`. A changed frame fails with a
//! diff; `INSTA_UPDATE=always cargo test -p dfdu-tui` rewrites the snapshots, so a reviewer sees
//! the change in the diff. With `CI` set, a missing snapshot fails instead of being written.

mod matrix;

use std::fs;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::Totals;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::App;
use crate::snapshot::{self, EntryData, Mark, Snapshot, Target, test_entry};
use crate::theme::Theme;
use crate::view;

/// The frame as text, one line per row with trailing spaces removed. On Windows a path joined
/// to the tests' Unix-style roots has `\` before its last components (`/r\proj`); it reads as
/// `/` here so the frames and snapshots are the same on every platform.
pub(crate) fn text(buf: &Buffer) -> String {
    let w = buf.area.width as usize;
    let mut out = String::new();
    for (i, cell) in buf.content.iter().enumerate() {
        match cell.symbol() {
            "\\" if cfg!(windows) => out.push('/'),
            s => out.push_str(s),
        }
        if (i + 1) % w == 0 {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
    }
    out
}

pub(crate) fn render_buffer(app: &mut App, now: Instant, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    term.draw(|f| view::draw(f, app, now)).expect("draw");
    term.backend().buffer().clone()
}

pub(crate) fn render(app: &mut App, now: Instant, w: u16, h: u16) -> String {
    text(&render_buffer(app, now, w, h))
}

/// The frame with its styles, for snapshots that check colour. Each line of text is followed,
/// unless all its cells have the default style, by `L<row>` and its runs of equally styled
/// cells, separated by ` | `: the columns `<first>-<last>` (both included), then the
/// foreground, background and modifiers that differ from the default, as in
/// `L3 0-6 bold | 8-31 fg=cyan | 40-46 fg=red bold`. Runs of default cells are left out.
/// Colours are ratatui's names in lower case, `i<n>` when indexed and `#rrggbb` when RGB.
pub(crate) fn styled(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for (row, line) in (area.top()..area.bottom()).zip(text(buf).lines()) {
        out.push_str(line);
        out.push('\n');
        let mut runs = Vec::new();
        let mut x = area.left();
        while x < area.right() {
            let (start, run) = (x, look(&buf[(x, row)]));
            while x < area.right() && look(&buf[(x, row)]) == run {
                x += 1;
            }
            if run != look(&Cell::EMPTY) {
                let (first, last) = (start - area.x, x - 1 - area.x);
                runs.push(format!("{first}-{last} {}", describe(run)));
            }
        }
        if !runs.is_empty() {
            out.push_str(&format!("L{} {}\n", row - area.y, runs.join(" | ")));
        }
    }
    out
}

/// What `styled` shows of a cell's style.
type Look = (Color, Color, Modifier);

fn look(cell: &Cell) -> Look {
    (cell.fg, cell.bg, cell.modifier)
}

fn describe((fg, bg, modifier): Look) -> String {
    let mut parts = Vec::new();
    if fg != Color::Reset {
        parts.push(format!("fg={}", colour(fg)));
    }
    if bg != Color::Reset {
        parts.push(format!("bg={}", colour(bg)));
    }
    parts.extend(modifier.iter_names().map(|(name, _)| name.to_lowercase()));
    parts.join(" ")
}

fn colour(c: Color) -> String {
    match c {
        Color::Indexed(n) => format!("i{n}"),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        c => format!("{c:?}").to_lowercase(),
    }
}

/// The cells of the first occurrence of `needle`, looked for row by row; panics with the frame
/// if it is not there.
#[track_caller]
fn find(buf: &Buffer, needle: &str) -> Rect {
    for y in buf.area.top()..buf.area.bottom() {
        let mut row = String::new();
        let mut starts = Vec::new();
        for x in buf.area.left()..buf.area.right() {
            starts.push((row.len(), x));
            row.push_str(buf[(x, y)].symbol());
        }
        if let Some(at) = row.find(needle) {
            let mut cells = starts
                .iter()
                .filter(|&&(i, _)| (at..at + needle.len()).contains(&i))
                .map(|&(_, x)| x);
            let first = cells.next().expect("a cell starts where the match does");
            let last = cells.next_back().unwrap_or(first);
            return Rect::new(first, y, last - first + 1, 1);
        }
    }
    panic!("{needle:?} is not in the frame:\n{}", text(buf));
}

/// The style of the first cell of the first occurrence of `needle`.
#[track_caller]
pub(crate) fn style_at(buf: &Buffer, needle: &str) -> Style {
    let at = find(buf, needle);
    buf[(at.x, at.y)].style()
}

/// Asserts that every cell of the first occurrence of `needle` has a style that satisfies
/// `pred`.
#[track_caller]
pub(crate) fn assert_cue(buf: &Buffer, needle: &str, pred: impl Fn(Style) -> bool) {
    let at = find(buf, needle);
    for x in at.left()..at.right() {
        let style = buf[(x, at.y)].style();
        assert!(
            pred(style),
            "{needle:?}: the cell at column {x}, row {} has {style:?}\n{}",
            at.y,
            styled(buf)
        );
    }
}

fn dir(i: u32, name: &str, size: u64, items: u64, mark: Mark) -> EntryData {
    EntryData {
        is_dir: true,
        items,
        mark,
        ..test_entry(i, name, size)
    }
}

/// A scan of a home directory 14 s in, with entries that are scanning, stalled and unreadable,
/// drawn on 100x12.
fn mid_scan(theme: Theme) -> Buffer {
    let (mut app, now) = mid_scan_app(theme);
    render_buffer(&mut app, now, 100, 12)
}

/// The browser on a scan of a home directory, with entries that are scanning, stalled and
/// unreadable, and the time 14 s into the scan.
pub(crate) fn mid_scan_app(theme: Theme) -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(theme, t0);
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
    (app, t0 + Duration::from_secs(14))
}

#[test]
fn mid_scan_view() {
    insta::assert_snapshot!("mid_scan", text(&mid_scan(Theme::no_color())));
}

#[test]
fn mid_scan_view_colours() {
    let buf = mid_scan(Theme::ansi16());
    insta::assert_snapshot!("mid_scan_ansi16", styled(&buf));
    let reversed = |s: Style| s.add_modifier.contains(Modifier::REVERSED);
    // The selection is the first row.
    assert_cue(&buf, "112.0 GiB", reversed);
    assert_cue(&buf, "Library/", reversed);
    assert!(!reversed(style_at(&buf, "78.0 GiB")));
    for name in ["Library/", "dev/", ".android/", "locked/", "nfs/"] {
        assert_cue(&buf, name, |s| {
            s.fg == Some(Color::Blue) && s.add_modifier.contains(Modifier::BOLD)
        });
    }
    assert_cue(&buf, "notes.sqlite", |s| s.fg == Some(Color::Reset));
    assert_cue(&buf, "permission denied", |s| s.fg == Some(Color::Red));
    assert_cue(&buf, "~", |s| s.fg == Some(Color::Yellow));
    assert_cue(&buf, "not responding", |s| s.fg == Some(Color::Yellow));
}

/// A 10x2 buffer: two styled runs and a styled cell after a gap on the first row, plain text
/// and a styled blank cell on the second.
fn sample_buffer() -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
    buf.set_string(0, 0, "ab", Style::new().add_modifier(Modifier::BOLD));
    buf.set_string(2, 0, "cde", Style::new().fg(Color::Cyan));
    buf.set_string(
        7,
        0,
        "x",
        Style::new()
            .fg(Color::Indexed(178))
            .bg(Color::Rgb(1, 2, 255))
            .add_modifier(Modifier::ITALIC | Modifier::REVERSED),
    );
    buf.set_string(8, 0, "y", Style::new().fg(Color::LightRed));
    buf.set_string(0, 1, "plain", Style::new());
    buf.set_style(Rect::new(9, 1, 1, 1), Style::new().bg(Color::DarkGray));
    buf
}

#[test]
fn styled_dump_merges_runs() {
    assert_eq!(
        styled(&sample_buffer()),
        "abcde  xy\n\
         L0 0-1 bold | 2-4 fg=cyan | 7-7 fg=i178 bg=#0102ff italic reversed | 8-8 fg=lightred\n\
         plain\n\
         L1 9-9 bg=darkgray\n"
    );
    let mut plain = Buffer::empty(Rect::new(0, 0, 4, 1));
    plain.set_string(0, 0, "abc", Style::new());
    assert_eq!(styled(&plain), "abc\n");
}

#[test]
fn style_at_finds_text() {
    let buf = sample_buffer();
    assert_eq!(style_at(&buf, "cde").fg, Some(Color::Cyan));
    assert_eq!(style_at(&buf, "de").fg, Some(Color::Cyan));
    // The first cell decides, though the needle runs on into other styles.
    assert!(style_at(&buf, "bcd").add_modifier.contains(Modifier::BOLD));
    assert_eq!(style_at(&buf, "lain").fg, Some(Color::Reset));
    assert_cue(&buf, "cde", |s| s.fg == Some(Color::Cyan));
    let missing = std::panic::catch_unwind(|| style_at(&buf, "absent"));
    let msg = missing.expect_err("absent text panics");
    let msg = msg.downcast_ref::<String>().expect("message");
    assert!(msg.contains("abcde  xy\nplain"), "{msg}");
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
        kinds: vec![(dfdu_tree::ErrKind::PermissionDenied, 60)],
    };
    for overlay in [
        crate::app::Overlay::Help(Default::default()),
        crate::app::Overlay::Errors { list, cursor: 49 },
        crate::app::Overlay::ConfirmDelete(crate::confirm::Confirm::new(
            vec![crate::confirm::Item {
                node,
                name: "junk".into(),
                size: 5 << 20,
                items: 1,
                shared_out: 1 << 20,
                is_dir: true,
                scanning: false,
                inside: false,
            }],
            dfdu_engine::Mode::Permanent,
        )),
    ] {
        app.overlay = overlay;
        // Smaller than either dialog: the dialog is clamped to the screen.
        let out = render(&mut app, t0, 40, 8);
        assert_eq!(out.lines().count(), 8, "{out}");
        let out = render(&mut app, t0, 100, 30);
        assert!(out.contains('┌') || out.contains('┏'), "{out}");
        if matches!(app.overlay, crate::app::Overlay::Errors { .. }) {
            // Scrolled to the cursor, with the count of what is not listed.
            assert!(out.contains("number/49/  permission denied"), "{out}");
            assert!(out.contains("first 50 shown"), "{out}");
        }
    }
}

/// A finished scan of `/r`: files `a` and `b` of 1000 bytes each, which hold 400 bytes alone
/// and share 600 more, counted at the directory.
pub(crate) fn shared_model() -> dfdu_engine::Model {
    use dfdu_tree::{Kind, Meta};
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
    let mut m = dfdu_engine::Model::default();
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
        m.tree.set_measured(f, 400, false);
    }
    m.tree.add_home(root, 600, false);
    m
}

#[test]
fn shared_data_has_its_own_row_and_cannot_be_deleted() {
    let m = shared_model();
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
        app.key(key(KeyCode::Down), t0);
    }
    app.key(key(KeyCode::Char('D')), t0);
    assert!(matches!(app.overlay, crate::app::Overlay::None));
}

pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn errors_are_listed_and_lead_to_the_entry() {
    use dfdu_tree::{ErrKind, Kind, Meta};
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
    let mut m = dfdu_engine::Model::default();
    let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 1), true);
    let a = m.tree.add_dir_flagged(root, b"a", &meta(Kind::Dir, 2), 0);
    let b = m.tree.add_dir_flagged(a, b"b", &meta(Kind::Dir, 3), 0);
    let locked = m.tree.add_dir_flagged(b, b"locked", &meta(Kind::Dir, 4), 0);
    let gone = m
        .tree
        .add_file(root, b"gone", &meta(Kind::File, 5), false, None);
    for d in [root, a, b, locked] {
        m.tree.dir_mut(d).unwrap().state = dfdu_tree::DirState::Complete;
    }
    m.tree.dir_mut(locked).unwrap().state = dfdu_tree::DirState::Failed;
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
    use dfdu_sys::{Disk, FsClass, Space};
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
    insta::assert_snapshot!("disk_picker", out);
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
