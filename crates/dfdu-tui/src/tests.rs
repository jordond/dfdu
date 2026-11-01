//! The shared test helpers: frames rendered with ratatui's `TestBackend`, the styles of their
//! cells, keys, and the snapshots, trees and apps the tests of several modules start from. The
//! frame tests of the browser as a whole are in `frames`.
//!
//! Frames are compared with insta snapshots in the `snapshots/` directory beside each test. A
//! changed frame fails with a diff; `INSTA_UPDATE=always cargo test -p dfdu-tui` rewrites the
//! snapshots, so a reviewer sees the change in the diff. With `CI` set, a missing snapshot fails
//! instead of being written.

mod blocked;
#[cfg(target_os = "linux")]
mod btrfs;
mod columns;
mod frames;
mod matrix;
mod navigation;
mod read_only;
mod review;
mod soak;
mod tree;
mod tree_frames;

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dfdu_engine::Model;
pub(crate) use dfdu_engine::test_support::meta;
use dfdu_engine::test_support::{add_dir, add_file, node_at};
pub(crate) use dfdu_session::test_support::root_model;
use dfdu_tree::{DirState, HostPath, Ix, Kind, Totals};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, Overlay};
use crate::model::extras;
use crate::model::snapshot::{self, EntryData, Mark, Snapshot, test_entry};
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
            let _ = writeln!(out, "L{} {}", row - area.y, runs.join(" | "));
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
        parts.push(format!("fg={}", color_name(fg)));
    }
    if bg != Color::Reset {
        parts.push(format!("bg={}", color_name(bg)));
    }
    parts.extend(modifier.iter_names().map(|(name, _)| name.to_lowercase()));
    parts.join(" ")
}

fn color_name(c: Color) -> String {
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
        dir_path: HostPath::from("/home/jo"),
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

/// A finished scan of `/r`: files `a` and `b` of 1000 bytes each, which hold 400 bytes alone
/// and share 600 more, counted at the directory.
pub(crate) fn shared_model() -> Model {
    let (mut m, root) = root_model();
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
    m.tree.add_shared(root, 600, false);
    m
}

/// Types `s` into whatever has the keyboard (an overlay's search, the filter), checking that no
/// key had an effect for the run loop.
pub(crate) fn typed(app: &mut App, s: &str, now: Instant) {
    for c in s.chars() {
        assert!(app.key(press(KeyCode::Char(c)), now).is_empty());
    }
}

/// The names of the list rows, in display order.
pub(crate) fn row_names(app: &App) -> Vec<String> {
    app.list
        .rows
        .iter()
        .map(|&i| String::from_utf8_lossy(&app.snap.entries[i].name).into_owned())
        .collect()
}

/// Selects the row named `name`, as a cursor move of the user's.
#[track_caller]
pub(crate) fn select(app: &mut App, name: &str) {
    let row = row_names(app)
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} is not a row: {:?}", row_names(app)));
    app.move_to(row);
}

/// The paths of the marked entries, in the order they were marked.
pub(crate) fn marked_paths(app: &App) -> Vec<HostPath> {
    (app.marks.items().iter())
        .map(|it| it.target.path.clone())
        .collect()
}

/// The names of the marked entries, in the order they were marked.
pub(crate) fn marked_names(app: &App) -> Vec<String> {
    (app.marks.items().iter())
        .map(|it| {
            String::from_utf8_lossy(it.target.path.file_name().unwrap_or_default()).into_owned()
        })
        .collect()
}

/// Presses `code` where it would delete, checking that no dialog opened and that the status
/// line says why, with `says` in it.
#[track_caller]
pub(crate) fn refused(app: &mut App, code: KeyCode, says: &str, now: Instant) {
    app.key(press(code), now);
    assert!(matches!(app.overlay, Overlay::None), "a dialog opened");
    let status = app.status.as_deref().unwrap_or_default();
    assert!(status.contains(says), "{status:?} does not say {says:?}");
}

/// The path of `rel` (names separated by `/`) below the root `root`, as a tree of this
/// machine builds it: with this machine's separator.
pub(crate) fn below(root: &str, rel: &str) -> HostPath {
    rel.split('/')
        .fold(HostPath::from(root), |p, n| p.join(n.as_bytes()))
}

/// A press of `code` without modifiers.
pub(crate) fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A model of `/r` built up one directory or file at a time, each with an inode of its own.
pub(crate) struct Fixture {
    pub(crate) m: Model,
    ino: u64,
}

impl Fixture {
    pub(crate) fn new() -> (Self, Ix) {
        let (m, root) = root_model();
        (Fixture { m, ino: 1 }, root)
    }

    pub(crate) fn dir(&mut self, parent: Ix, name: &str) -> Ix {
        self.ino += 1;
        add_dir(&mut self.m.tree, parent, name, self.ino)
    }

    pub(crate) fn file(&mut self, parent: Ix, name: &str, size: u64) -> Ix {
        self.ino += 1;
        add_file(&mut self.m.tree, parent, name, size, self.ino)
    }
}

/// `/r` with `big/` (`a/` with two files, and `b`), `mid/` (three files), `small` and dotfile
/// `.hid`.
pub(crate) fn tree() -> Fixture {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let big = fx.dir(root, "big");
    let a = fx.dir(big, "a");
    fx.file(a, "a1", 30_000);
    fx.file(a, "a2", 10_000);
    fx.file(big, "b", 20_000);
    let mid = fx.dir(root, "mid");
    for (i, size) in [12_000, 8_000, 5_000].into_iter().enumerate() {
        fx.file(mid, &format!("m{i}"), size);
    }
    fx.file(root, "small", 9_000);
    fx.file(root, ".hid", 4_000);
    fx
}

/// [`tree`] with the built-in rules, `big` matched as a Rust `target/` and `mid/m0` as a
/// directory with a `CACHEDIR.TAG`.
pub(crate) fn classified() -> Fixture {
    let mut fx = tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = dfdu_rules::builtin(&env).0;
    let class = dfdu_tree::Class {
        rule: rules.index("rust.target").unwrap(),
        safety: dfdu_tree::Safety::Regenerable,
        degraded: None,
        flags: dfdu_tree::ClassFlags::NONE,
    };
    let cachedir = dfdu_tree::Class {
        rule: rules.index("generic.cachedir-tag").unwrap(),
        safety: dfdu_tree::Safety::Safe,
        ..class
    };
    fx.m.rules = Some(Arc::new(rules));
    let (big, m0) = (node_at(&fx.m.tree, "big"), node_at(&fx.m.tree, "mid/m0"));
    fx.m.tree.set_class(big, Some(class));
    fx.m.tree.set_class(m0, Some(cachedir));
    fx
}

/// A browser of [`classified`] read once at `t0`, and the fixture it reads.
pub(crate) fn classified_app(t0: Instant) -> (Fixture, App) {
    let fx = classified();
    let mut app = App::new(Theme::no_color(), t0);
    sync(&mut app, &fx.m, t0);
    (fx, app)
}

/// Reads the model as the run loop does for a frame with a new snapshot: the snapshot, then
/// the extras until none is stale.
pub(crate) fn sync(app: &mut App, m: &Model, now: Instant) {
    let mut target = app.target.clone();
    let snap = snapshot::build(m, &mut target, app.size_mode);
    app.target = target;
    app.refresh(snap, now);
    for _ in 0..3 {
        let budget = dfdu_scan::Budget::new(1 << 20);
        let x = extras::read(m, app, &budget, crate::features::debug::Counters::default());
        extras::apply(app, x);
        if !app.extras_stale() {
            return;
        }
    }
    panic!("the extras stay stale");
}

/// A snapshot of `/r` while it is scanned, holding `entries`, with its totals left at zero.
pub(crate) fn snap_without_totals(entries: Vec<EntryData>, version: u64) -> Snapshot {
    let root = test_entry(99, "/r", 0);
    Snapshot {
        dir_path: HostPath::from("/r"),
        dir_node: Some(root.node),
        at_root: true,
        entries,
        busy: true,
        version,
        ..Default::default()
    }
}

/// A snapshot of `/r` while it is scanned, holding `entries`, with their sizes and count as the
/// totals of the directory and of the scan.
pub(crate) fn snap_with_totals(entries: Vec<EntryData>, version: u64) -> Snapshot {
    let size = entries.iter().map(|e| e.size).sum();
    let totals = Totals {
        apparent: size,
        alloc: size,
        items: entries.len() as u64 + 1,
        ..Totals::default()
    };
    Snapshot {
        dir_total: totals,
        root_total: totals,
        ..snap_without_totals(entries, version)
    }
}

/// [`snap_with_totals`] of `Library/` (3 GiB) and `notes.sqlite` (625 MiB).
pub(crate) fn library_snap() -> Snapshot {
    let entries = vec![
        EntryData {
            is_dir: true,
            ..test_entry(0, "Library", 3 << 30)
        },
        test_entry(1, "notes.sqlite", 625 << 20),
    ];
    snap_with_totals(entries, 1)
}
