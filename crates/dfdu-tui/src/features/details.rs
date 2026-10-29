//! The details pane (`i`): everything the tree knows about the selected entry. On the right at
//! 100 columns or more, as a sheet below the list when narrower. `Tab` gives it the focus, and
//! the arrow and page keys then scroll it.

mod text;

use std::path::PathBuf;

use dfdu_engine::{Model, SizeMode, subvol_label};
use dfdu_tree::{Ix, Kind, NodeFlags, NodeRef, RuleIx, Safety, Totals, Tree};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::{App, Effect};
use crate::model::snapshot::Mark;
use crate::theme::{count, human};
use crate::view::cells;

use text::lines;
#[cfg(test)]
use text::{state, wrap};

/// The pane goes on the right from this width of the area below the header...
const RIGHT_FROM: u16 = 100;
/// ...this wide, or a third of the width if that is more.
const RIGHT_WIDTH: u16 = 40;
/// Otherwise it is a sheet this high below the list...
const SHEET_HEIGHT: u16 = 10;
/// ...if the list keeps this many rows.
const LIST_MIN: u16 = 6;
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
    /// What `app.extras.details` was read for.
    read_for: ReadFor,
    /// The summary [`split`] put in the status line in place of a pane that does not fit, to
    /// replace as the selection moves.
    summary: Option<String>,
}

/// What the details were last read for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ReadFor {
    /// Nothing read since the pane opened.
    #[default]
    Unread,
    /// The selected entry, or no entry.
    Entry(Option<NodeRef>),
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
    /// Its own error ([`dfdu_engine::problem`]).
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
    pub safety: Safety,
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
    let (at, c) = rules.class_in_effect(&m.tree, ix)?;
    let r = rules.rule(c.rule);
    let cat = r.category();
    Some(Matched {
        rule: r.name.clone(),
        category: cat.name,
        slot: cat.color as usize,
        safety: c.safety,
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
    let mut add = |cx: Ix, rule: RuleIx| {
        let cat = rules.rule(rule).category();
        let size = mode.of(&tree.total(cx));
        match by.iter_mut().find(|b| b.0 == cat.name) {
            Some(b) => (b.2, b.3) = (b.2 + size, b.3 + 1),
            None => by.push((cat.name, cat.color as usize, size, 1)),
        }
    };
    if let Some((at, c)) = rules.class_in_effect(tree, ix) {
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
        .map_or(1, |r| i32::from(r.height.saturating_sub(2).max(1)));
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

/// The selected row in one line, for the status line when the pane does not fit: its name,
/// size and, for a directory, its items.
fn summary(app: &App) -> Option<String> {
    let (name, is_dir, size, items) = if crate::features::cleanup::active(app) {
        let it = crate::features::cleanup::selected_item(app)?;
        let name = it.path.file_name().unwrap_or(it.path.as_os_str());
        let is_dir = it.kind == Kind::Dir;
        (
            name.to_string_lossy().into_owned(),
            is_dir,
            it.size,
            it.items,
        )
    } else {
        let e = app.selected_entry()?;
        let name = String::from_utf8_lossy(&e.name).into_owned();
        (name, e.is_dir, e.size, e.items)
    };
    Some(if is_dir {
        format!("▸ {name}/  {} · {} items", human(size), count(items))
    } else {
        format!("▸ {name}  {}", human(size))
    })
}

/// Splits the area below the header into the list and the pane, if the pane is open and fits.
/// When it does not fit, the list view's selection line already sums up the selected row; the
/// other views show a summary in the status line instead, unless it has another message to show.
pub fn split(app: &mut App, area: Rect, list_view: bool) -> (Rect, Option<Rect>) {
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
    let ours = app.status.is_none() || app.status == app.details.summary;
    if let Some([list, pane]) = parts {
        if ours {
            app.status = None;
        }
        app.details.summary = None;
        (list, Some(pane))
    } else {
        app.details.focused = false;
        let line = if list_view { None } else { summary(app) };
        if ours {
            app.status.clone_from(&line);
        }
        app.details.summary = line;
        (area, None)
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
        block = block.title_bottom(Line::styled(" ↓ more ", t.fg_dim).right_aligned());
    }
    f.render_widget(
        Paragraph::new(lines)
            .scroll((cells(scroll), 0))
            .block(block),
        area,
    );
}

/// True when the pane shows something other than what [`read`] last read. A read ([`read`],
/// then [`apply`]) must make it false: while it is true, the run loop reads again at once.
pub fn stale(app: &App) -> bool {
    app.details.open && app.details.read_for != ReadFor::Entry(subject(app).map(|e| e.node))
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
    if crate::features::cleanup::active(app) {
        let it = crate::features::cleanup::selected_item(app)?;
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
            problem: dfdu_engine::problem(tree, ix),
            errors_below: dfdu_engine::errors_below(tree, ix),
            mtime: tree.mtime(ix),
            dev_ino: tree.dev_ino(ix),
            mark: e.mark,
            label: subvol_label(m, ix),
            inside: {
                let (t, flags) = (tree.total(ix), tree.flags(ix));
                (
                    t.mounts
                        .saturating_sub(u64::from(flags.has(NodeFlags::MOUNT_ROOT))),
                    t.ro_subvols
                        .saturating_sub(u64::from(flags.has(NodeFlags::RO_SUBVOL))),
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
    if app.details.read_for != ReadFor::Entry(node) {
        app.details.scroll = 0;
    }
    app.details.read_for = ReadFor::Entry(node);
    let row = subject(app)
        .filter(|e| Some(e.node) == node)
        .map(|e| (e.mark, e.compressed));
    if let (Some((mark, compressed)), Some(f)) = (row, &mut app.extras.details.facts) {
        f.mark = mark;
        f.compressed = compressed;
    }
}

#[cfg(test)]
mod tests;
