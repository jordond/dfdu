//! The details pane (`i`): everything the tree knows about the selected entry. On the right at
//! 100 columns or more, as a sheet below the list when narrower. `Tab` gives it the focus, and
//! the arrow and page keys then scroll it.

use std::path::PathBuf;

use dfdu_engine::Model;
use dfdu_tree::{Ix, Kind, NodeFlags, NodeRef, Totals, Tree};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::{App, Effect};
use crate::snapshot::{Mark, SizeMode, subvol_label};
use crate::theme::{bar, count, human};

/// The pane goes on the right from this width of the area below the header...
const RIGHT_FROM: u16 = 100;
/// ...this wide, or a third of the width if that is more.
const RIGHT_WIDTH: u16 = 40;
/// Otherwise it is a sheet this high below the list...
const SHEET_HEIGHT: u16 = 10;
/// ...if the list keeps this many rows.
const LIST_MIN: u16 = 6;
const TOO_SMALL: &str = "terminal too small for details";
/// Children listed as the largest.
const LARGEST: usize = 5;
/// Directories with more children than this get no list of the largest: finding them reads
/// every child under the read lock.
const LARGEST_MAX_CHILDREN: usize = 100_000;
const BAR: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct Details {
    pub open: bool,
    focused: bool,
    /// First line shown.
    scroll: usize,
    /// The entry `app.extras.details` was read for; `None` until a read since the pane opened.
    read_for: Option<Option<NodeRef>>,
}

/// What [`read`] found for the selected entry under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Info {
    /// The selected entry when it was read.
    pub node: Option<NodeRef>,
    /// `None` when nothing is selected, the pane is closed, or the entry is not a node of its
    /// own (the "shared between entries" row, or a deleted entry).
    pub facts: Option<Facts>,
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub is_dir: bool,
    pub total: Totals,
    /// Compressed extents of unknown size on disk: the disk usage is an upper bound.
    pub compressed: bool,
    /// Its own error ([`crate::problem`]).
    pub problem: Option<String>,
    pub errors_below: u64,
    pub mtime: i64,
    pub dev_ino: (u64, u64),
    /// How the list marks it, which for a directory is its scan state.
    pub mark: Mark,
    /// btrfs subvolume label ([`subvol_label`]).
    pub label: Option<String>,
    /// Mounts and read-only subvolumes below it, where deletion stops.
    pub inside: (u64, u64),
    pub largest: Largest,
    /// What the sizes of `largest` are.
    pub size_mode: SizeMode,
}

#[derive(Clone, Debug)]
pub enum Largest {
    /// Not a directory.
    None,
    /// The largest children, largest first.
    Listed(Vec<Child>),
    /// A directory with this many children, too many to look through.
    Skipped(usize),
}

#[derive(Clone, Debug)]
pub struct Child {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Opens or closes the pane.
pub fn toggle(app: &mut App) {
    let d = &mut app.details;
    *d = Details {
        open: !d.open,
        ..Details::default()
    };
}

/// Moves the focus between the list and the pane, when the pane is shown.
pub fn focus(app: &mut App) {
    if app.details.open && app.areas.details.is_some() {
        app.details.focused = !app.details.focused;
    }
}

/// True while the pane has the focus.
pub fn focused(app: &App) -> bool {
    app.details.open && app.details.focused
}

/// Handles a key while the pane has the focus; `None` passes it on to the list.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let page = app
        .areas
        .details
        .map_or(1, |r| r.height.saturating_sub(2).max(1) as i32);
    match k.code {
        KeyCode::Up => scroll(app, -1),
        KeyCode::Down => scroll(app, 1),
        KeyCode::PageUp => scroll(app, -page),
        KeyCode::PageDown => scroll(app, page),
        KeyCode::Esc | KeyCode::Tab => {
            app.details.focused = false;
            true
        }
        KeyCode::Char('i') => {
            toggle(app);
            true
        }
        _ => false,
    }
    .then(Vec::new)
}

/// Scrolls the pane by `delta` lines, focused or not (the mouse wheel over it). False when the
/// pane is not shown, so the caller can move the list instead.
pub(crate) fn scroll(app: &mut App, delta: i32) -> bool {
    let Some(r) = app.areas.details.filter(|_| app.details.open) else {
        return false;
    };
    let inner = Block::bordered().inner(r);
    let max = lines(app, inner.width as usize)
        .len()
        .saturating_sub(inner.height as usize);
    app.details.scroll = app
        .details
        .scroll
        .saturating_add_signed(delta as isize)
        .min(max);
    true
}

/// Splits the area below the header into the list and the pane, if the pane is open and fits.
/// When it does not fit, the status line says so.
pub fn split(app: &mut App, area: Rect) -> (Rect, Option<Rect>) {
    if !app.details.open {
        return (area, None);
    }
    let parts = if area.width >= RIGHT_FROM {
        let w = RIGHT_WIDTH.max(area.width / 3);
        Some(Layout::horizontal([Constraint::Min(1), Constraint::Length(w)]).areas(area))
    } else if area.height >= SHEET_HEIGHT + LIST_MIN {
        Some(Layout::vertical([Constraint::Min(1), Constraint::Length(SHEET_HEIGHT)]).areas(area))
    } else {
        None
    };
    match parts {
        Some([list, pane]) => {
            if app.status.as_deref() == Some(TOO_SMALL) {
                app.status = None;
            }
            (list, Some(pane))
        }
        None => {
            app.details.focused = false;
            app.status = Some(TOO_SMALL.into());
            (area, None)
        }
    }
}

/// Draws the pane, titled with the entry's name: a rounded border, heavy while it has the focus.
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let (border, style) = if focused(app) {
        (BorderType::Thick, t.border_focus)
    } else {
        (BorderType::Rounded, t.border)
    };
    let title = match &app.extras.details.facts {
        Some(f) => format!(" {}{} ", f.name, if f.is_dir { "/" } else { "" }),
        None => " Details ".into(),
    };
    let mut block = Block::bordered()
        .border_type(border)
        .border_style(style)
        .title(Span::styled(title, t.title));
    let inner = block.inner(area);
    let lines = lines(app, inner.width as usize);
    let max = lines.len().saturating_sub(inner.height as usize);
    let scroll = app.details.scroll.min(max);
    if scroll < max {
        block = block.title_bottom(Line::styled(" ↓ more ", t.dim).right_aligned());
    }
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll as u16, 0))
            .block(block),
        area,
    );
}

/// True when the pane shows something other than what [`read`] last read. A read ([`read`],
/// then [`apply`]) must make it false: while it is true, the run loop reads again at once.
pub fn stale(app: &App) -> bool {
    app.details.open && app.details.read_for != Some(app.selected_entry().map(|e| e.node))
}

/// Reads the selected entry's details; runs under the read lock. Costs a walk up to the root
/// for the path and, for a directory of at most [`LARGEST_MAX_CHILDREN`] children, a look at
/// each child; never a walk of the subtree.
pub fn read(m: &Model, app: &App) -> Info {
    read_within::<LARGEST_MAX_CHILDREN>(m, app)
}

/// [`read`], listing the largest children only of directories with at most `MAX` children.
fn read_within<const MAX: usize>(m: &Model, app: &App) -> Info {
    if !app.details.open {
        return Info::default();
    }
    let Some(e) = app.selected_entry() else {
        return Info::default();
    };
    let tree = &m.tree;
    let facts = match tree.resolve_visible(e.node) {
        Some(ix) if !e.pseudo => Some(Facts {
            name: String::from_utf8_lossy(tree.name(ix)).into_owned(),
            path: tree.path_buf(ix),
            kind: tree.kind(ix),
            is_dir: tree.dir(ix).is_some(),
            total: tree.total(ix),
            compressed: e.compressed,
            problem: crate::problem(tree, ix),
            errors_below: crate::errors_below(tree, ix),
            mtime: tree.mtime(ix),
            dev_ino: tree.dev_ino(ix),
            mark: e.mark,
            label: subvol_label(m, ix),
            inside: {
                let (t, flags) = (tree.total(ix), tree.flags(ix));
                (
                    t.mounts
                        .saturating_sub(flags.has(NodeFlags::MOUNT_ROOT) as u64),
                    t.ro_subvols
                        .saturating_sub(flags.has(NodeFlags::RO_SUBVOL) as u64),
                )
            },
            largest: largest::<MAX>(tree, ix, app.size_mode),
            size_mode: app.size_mode,
        }),
        _ => None,
    };
    Info {
        node: Some(e.node),
        facts,
    }
}

fn largest<const MAX: usize>(tree: &Tree, ix: Ix, mode: SizeMode) -> Largest {
    if tree.dir(ix).is_none() {
        return Largest::None;
    }
    let n = tree.children(ix).len();
    if n > MAX {
        return Largest::Skipped(n);
    }
    let mut top: Vec<(u64, Ix)> = Vec::with_capacity(LARGEST + 1);
    for c in tree.children(ix) {
        let size = mode.of(&tree.total(c));
        if top.len() < LARGEST || size > top[LARGEST - 1].0 {
            let at = top.partition_point(|&(s, _)| s >= size);
            top.insert(at, (size, c));
            top.truncate(LARGEST);
        }
    }
    Largest::Listed(
        top.into_iter()
            .map(|(size, c)| Child {
                name: String::from_utf8_lossy(tree.name(c)).into_owned(),
                size,
                is_dir: tree.dir(c).is_some(),
            })
            .collect(),
    )
}

/// Takes in what [`read`] found, now in `app.extras.details`, after every read (with a new
/// snapshot or without). Another entry starts at the top.
pub fn apply(app: &mut App) {
    let node = app.extras.details.node;
    if app.details.read_for != Some(node) {
        app.details.scroll = 0;
    }
    app.details.read_for = Some(node);
}

/// The scan state of a directory, by the rules of its mark in the list.
fn state(mark: Mark) -> &'static str {
    match mark {
        Mark::Scanning => "scanning",
        Mark::Stalled => "stalled: not responding",
        Mark::Rescanning => "rescanning",
        Mark::Error => "incomplete",
        Mark::Incomplete => "incomplete: scan stopped, r to rescan",
        Mark::Deleting => "being deleted",
        Mark::OtherFs => "not scanned: other filesystem",
        Mark::DupMount => "not scanned: mounted again",
        Mark::Network => "not scanned: network filesystem",
        Mark::Kernel => "not scanned: kernel filesystem",
        Mark::ErrorsInside | Mark::None => "complete",
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Dir => "directory",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

/// Seconds since the Unix epoch as a UTC date and time ("2026-10-24 09:42 UTC").
fn date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to a civil date, in 400-year eras starting on 1 March.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// `text` in lines of at most `width` characters, broken at spaces; a word longer than a line
/// (a path) is cut.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut len = 0;
    for word in text.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        if len > 0 && len + 1 + word.len() <= width {
            line.push(' ');
            line.extend(&word);
            len += 1 + word.len();
            continue;
        }
        if len > 0 {
            out.push(std::mem::take(&mut line));
        }
        while word.len() > width {
            out.push(word.drain(..width).collect());
        }
        line = word.iter().collect();
        len = word.len();
    }
    out.push(line);
    out
}

/// The pane's lines for an inner width of `width`, most wanted first: where it is, what it is,
/// its sizes and problems, the largest children, then identity and boundaries.
fn lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut out = Vec::new();
    let text = |out: &mut Vec<Line<'static>>, s: &str, style: Style| {
        for piece in wrap(s, width.saturating_sub(1)) {
            out.push(Line::styled(format!(" {piece}"), style));
        }
    };
    let Some(f) = &app.extras.details.facts else {
        let note = match app.selected_entry() {
            Some(e) if e.ghost => "deleted".to_string(),
            Some(e) if e.pseudo => format!(
                "{} of data that entries of this directory share, counted here once. Not an \
                 entry: it cannot be opened or deleted.",
                human(e.size)
            ),
            _ => "nothing selected".to_string(),
        };
        text(&mut out, &note, t.dim);
        return out;
    };
    let plain = Style::new();
    text(&mut out, &f.path.display().to_string(), t.dim);
    if f.is_dir {
        let style = match f.mark {
            Mark::ErrorsInside | Mark::None => plain,
            Mark::Scanning | Mark::Rescanning => t.scan_active,
            Mark::Error => t.error,
            _ => t.warn,
        };
        let head = format!(" {} · ", kind_name(f.kind));
        let s = state(f.mark);
        if head.chars().count() + s.chars().count() <= width {
            out.push(Line::from(vec![Span::raw(head), Span::styled(s, style)]));
        } else {
            text(&mut out, kind_name(f.kind), plain);
            text(&mut out, s, style);
        }
    } else {
        text(&mut out, kind_name(f.kind), plain);
    }
    let mut disk = format!("{} on disk", human(f.total.alloc));
    if f.compressed {
        disk.push_str(" at most (compressed)");
    }
    if f.is_dir {
        disk.push_str(&format!(" · {}", plural(f.total.items, "item", "items")));
    }
    text(&mut out, &disk, plain);
    let other = format!(
        "{} apparent · {} like du",
        human(f.total.apparent),
        human(f.total.referenced)
    );
    text(&mut out, &other, plain);
    if f.total.shared_out > 0 {
        let s = format!(
            "{} also used outside the scan: deleting it frees less",
            human(f.total.shared_out)
        );
        text(&mut out, &s, t.warn);
    }
    if let Some(p) = &f.problem {
        text(&mut out, p, t.error);
    }
    if f.errors_below > 0 {
        let s = format!("{} below", plural(f.errors_below, "error", "errors"));
        text(&mut out, &s, t.error);
    }
    match &f.largest {
        Largest::None => {}
        Largest::Skipped(n) => {
            let s = format!(
                "largest children: not computed for {} entries",
                count(*n as u64)
            );
            text(&mut out, &s, t.dim);
        }
        Largest::Listed(kids) if kids.is_empty() => {}
        Largest::Listed(kids) => {
            text(
                &mut out,
                &format!("largest by {}", f.size_mode.label()),
                t.title,
            );
            let total = f.size_mode.of(&f.total).max(1);
            for k in kids {
                let style = if k.is_dir { t.dir } else { plain };
                out.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(bar(k.size as f64 / total as f64, BAR), t.bar),
                    Span::raw(format!(" {:>10}  ", human(k.size))),
                    Span::styled(
                        format!("{}{}", k.name, if k.is_dir { "/" } else { "" }),
                        style,
                    ),
                ]));
            }
        }
    }
    text(&mut out, &format!("modified {}", date(f.mtime)), plain);
    let id = format!("device {} · inode {}", f.dev_ino.0, f.dev_ino.1);
    text(&mut out, &id, plain);
    if let Some(l) = &f.label {
        text(&mut out, l, plain);
    }
    let (mounts, ro) = f.inside;
    if mounts + ro > 0 {
        let mut parts = Vec::new();
        if mounts > 0 {
            parts.push(plural(mounts, "mount", "mounts"));
        }
        if ro > 0 {
            parts.push(plural(ro, "read-only subvolume", "read-only subvolumes"));
        }
        let s = format!("inside: {}; deletion stops at them", parts.join(", "));
        text(&mut out, &s, t.warn);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_scan::Budget;
    use dfdu_tree::{DirState, ErrKind, Meta, NodeFlags};
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::app::keys::tests::press;
    use crate::debug::Counters;
    use crate::extras;
    use crate::snapshot::{self, Target};
    use crate::tests::render;
    use crate::theme::Theme;

    const MTIME: i64 = 1_792_834_920;

    fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
        Meta {
            kind,
            apparent: size,
            alloc: size,
            mtime: MTIME,
            dev: 1,
            ino,
            nlink: 1,
            mount: 0,
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
        tree.set_error(proj, ErrKind::Os(5));
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
            "partly read: input/output error",
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
    fn focus_is_visible_without_colour() {
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
}
