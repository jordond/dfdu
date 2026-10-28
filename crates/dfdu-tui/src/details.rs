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
    /// The rule it matched, or the match it is in.
    pub matched: Option<Matched>,
    /// The matches at or below it, by category: name, colour slot, size, count; largest first.
    pub categories: Vec<(&'static str, usize, u64, u64)>,
}

#[derive(Clone, Debug)]
pub struct Matched {
    pub rule: String,
    pub category: &'static str,
    pub slot: usize,
    pub safety: &'static str,
    /// The path of the match above, when it is not the entry itself.
    pub above: Option<PathBuf>,
    pub explanation: String,
    pub lost: String,
    /// The `degrade` condition that holds.
    pub degraded: Option<String>,
    pub flags: u8,
}

/// The rule node `ix` matched, or the match above it.
fn matched(m: &Model, ix: Ix) -> Option<Matched> {
    let rules = m.rules.as_ref()?;
    let (at, c) = m.tree.class_in_effect(ix)?;
    let r = rules.rule(c.rule);
    let cat = r.category();
    Some(Matched {
        rule: r.name.clone(),
        category: cat.name,
        slot: cat.colour as usize,
        safety: dfdu_rules::Safety::ALL[c.safety as usize].as_str(),
        above: (at != ix).then(|| m.tree.path_buf(at)),
        explanation: r.explanation.clone(),
        lost: r.lost.clone(),
        degraded: c.degraded.map(|d| r.degrade[d as usize].reason.clone()),
        flags: c.flags,
    })
}

/// The matches at or below `ix` that no other match covers, summed by category. Matches are
/// few (one per match, nothing below it), so this looks at all of them.
fn categories(m: &Model, ix: Ix, mode: SizeMode) -> Vec<(&'static str, usize, u64, u64)> {
    let Some(rules) = &m.rules else {
        return Vec::new();
    };
    let tree = &m.tree;
    let mut by: Vec<(&'static str, usize, u64, u64)> = Vec::new();
    let mut add = |cx: Ix, rule: u16| {
        let cat = rules.rule(rule).category();
        let size = mode.of(&tree.total(cx));
        match by.iter_mut().find(|b| b.0 == cat.name) {
            Some(b) => (b.2, b.3) = (b.2 + size, b.3 + 1),
            None => by.push((cat.name, cat.colour as usize, size, 1)),
        }
    };
    if let Some((at, c)) = tree.class_in_effect(ix) {
        add(at, c.rule);
    } else {
        for (cx, c) in tree.classes() {
            let (mut cur, mut covered) = (Some(cx), false);
            while let Some(i) = cur {
                if i == ix {
                    break;
                }
                covered |= i != cx && tree.class(i).is_some();
                cur = tree.parent(i);
            }
            if cur == Some(ix) && !covered && tree.is_visible(cx) {
                add(cx, c.rule);
            }
        }
    }
    by.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));
    by
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
/// When it does not fit, the status line says so, unless it has another message to show.
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
            app.status.get_or_insert_with(|| TOO_SMALL.into());
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
    app.details.open && app.details.read_for != Some(subject(app).map(|e| e.node))
}

/// What the pane describes.
struct Subject {
    node: NodeRef,
    pseudo: bool,
    compressed: bool,
    mark: Mark,
}

/// The cleanup view's selected item while that view is shown, else the list's selected entry.
fn subject(app: &App) -> Option<Subject> {
    if crate::cleanup::active(app) {
        let it = crate::cleanup::selected_item(app)?;
        return Some(Subject {
            node: it.node,
            pseudo: false,
            compressed: false,
            mark: if it.scanning {
                Mark::Scanning
            } else {
                Mark::None
            },
        });
    }
    app.selected_entry().map(|e| Subject {
        node: e.node,
        pseudo: e.pseudo,
        compressed: e.compressed,
        mark: e.mark,
    })
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
    let Some(e) = subject(app) else {
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
            matched: matched(m, ix),
            categories: if tree.dir(ix).is_some() {
                categories(m, ix, app.size_mode)
            } else {
                Vec::new()
            },
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
///
/// [`read`] runs before [`App::refresh`] takes in the snapshot read with it, so it saw the
/// previous snapshot's row: the mark and the compressed flag are taken again from the row the
/// list now draws for the same entry.
pub fn apply(app: &mut App) {
    let node = app.extras.details.node;
    if app.details.read_for != Some(node) {
        app.details.scroll = 0;
    }
    app.details.read_for = Some(node);
    let row = subject(app)
        .filter(|e| Some(e.node) == node)
        .map(|e| (e.mark, e.compressed));
    if let (Some((mark, compressed)), Some(f)) = (row, &mut app.extras.details.facts) {
        f.mark = mark;
        f.compressed = compressed;
    }
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
        Mark::ErrorsInside => "complete, errors below",
        Mark::None => "complete",
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
        let listed = app
            .selected_entry()
            .filter(|_| !crate::cleanup::active(app));
        let note = match listed {
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
            Mark::None => plain,
            Mark::ErrorsInside => t.dim,
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
    let size = app.size_mode.of(&f.total);
    if let Some((d, then, at)) = crate::growth::since(app, &f.path, size, f.mark) {
        let s = if d == 0 {
            format!("the same as on {}", dfdu_store::history::day(at))
        } else {
            format!("{} (was {})", crate::growth::describe(d, at), human(then))
        };
        text(&mut out, &s, if d > 0 { t.warn } else { t.dim });
    }
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
    if let Some(c) = &f.matched {
        out.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(c.category.to_string(), t.cat[c.slot]),
            Span::styled(format!(" · {}", c.safety), plain),
        ]));
        text(&mut out, &c.rule, t.title);
        if let Some(p) = &c.above {
            text(&mut out, &format!("in {}", p.display()), t.dim);
        }
        text(&mut out, &c.explanation, plain);
        text(&mut out, &format!("lost: {}", c.lost), t.dim);
        if let Some(d) = &c.degraded {
            text(&mut out, d, t.warn);
        }
        if c.flags & dfdu_tree::Class::VCS_TRACKED != 0 {
            text(
                &mut out,
                "version control tracks files in it: protected",
                t.warn,
            );
        }
        if c.flags & dfdu_tree::Class::IN_USE != 0 {
            text(&mut out, "a program that uses it was running", t.warn);
        }
        if c.flags & dfdu_tree::Class::VCS_UNKNOWN != 0 {
            text(
                &mut out,
                "git could not tell whether it tracks files in it: not recommended",
                t.warn,
            );
        }
    }
    // A match counts as one category; the breakdown is for directories with matches below.
    if !f.categories.is_empty() && f.matched.is_none() {
        text(&mut out, "by category", t.title);
        for &(name, slot, size, n) in &f.categories {
            out.push(Line::from(vec![
                Span::raw(format!(" {:>10}  ", human(size))),
                Span::styled(name.to_string(), t.cat[slot]),
                Span::styled(format!(" · {n}"), t.dim),
            ]));
        }
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
mod tests;
