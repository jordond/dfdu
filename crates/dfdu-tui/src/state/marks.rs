//! Marked entries: `Space` marks, the footer shows their live total, and a delete acts on them.
//! `v` marks a range as the cursor moves ([`range`]).
//!
//! In the list a marked directory can hold marked entries, which count and go with it. In tree
//! mode, where both show at once, a directory and something inside it are never both marked:
//! marking one unmarks the other ([`exclusive`]), as marking a container in the cleanup view
//! marks only its rest.
//!
//! A read-only browser marks nothing, as nothing marked could be deleted: every way of marking
//! says so on the status line instead.

use std::collections::{HashMap, HashSet};

use dfdu_engine::{JobKind, JobPhase, Model};
use dfdu_tree::{DirState, HostPath, Ident, Ix, NodeRef, Target, find_path};
use ratatui::text::Span;

use crate::app::App;
use crate::format::human;
use crate::model::snapshot::{EntryData, Mark};

pub(crate) mod range;

/// One marked entry: how to find it again, and its sizes as the last read found them.
#[derive(Clone, Debug)]
pub struct Item {
    /// The reference, with the path and identity that find it again after a rescan swap.
    pub target: Target,
    pub is_dir: bool,
    /// Size in the size mode of the last read.
    pub size: u64,
    pub items: u64,
    /// Part of `size` that data outside the scan also uses.
    pub shared_out: u64,
    /// A directory still being scanned: `size` is a lower bound.
    pub scanning: bool,
    /// Below another marked item: counted and deleted with that one.
    pub inside: bool,
}

/// The marked entries, in any directory, in the order they were marked.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    items: Vec<Item>,
    /// The references in `items`, for drawing rows.
    nodes: HashSet<NodeRef>,
    /// Counts the changes made to `items` by the user.
    version: u64,
    /// The `version` the last [`apply`] took in.
    applied: u64,
    /// The directory a range mark is going on in ([`range`]).
    range: Option<HostPath>,
}

impl Marks {
    /// The marked entries, in the order they were marked.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// True when `node` is marked.
    pub fn contains(&self, node: NodeRef) -> bool {
        self.nodes.contains(&node)
    }

    /// True when nothing is marked.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The total of the outermost items, and whether any of them is still being scanned.
    pub fn total(&self) -> (u64, bool) {
        self.items
            .iter()
            .filter(|it| !it.inside)
            .fold((0, false), |(n, scanning), it| {
                (n + it.size, scanning || it.scanning)
            })
    }

    /// Records a change by the user: the sizes need a read.
    fn record_change(&mut self) {
        self.version += 1;
        self.index();
    }

    /// Rebuilds `nodes` and which items are inside others: an item is inside when one of its
    /// ancestors is marked, so this costs a set lookup per mark and path component.
    fn index(&mut self) {
        self.nodes = self.items.iter().filter_map(|it| it.target.node).collect();
        let paths: HashSet<&HostPath> = self.items.iter().map(|it| &it.target.path).collect();
        let inside: Vec<bool> = self
            .items
            .iter()
            .map(|it| below_any(&it.target.path, &paths))
            .collect();
        for (it, inside) in self.items.iter_mut().zip(inside) {
            it.inside = inside;
        }
    }
}

/// Whether one of the directories `path` is in is among `paths`.
fn below_any(path: &HostPath, paths: &HashSet<&HostPath>) -> bool {
    let mut at = path.parent();
    while let Some(a) = at {
        if paths.contains(&a) {
            return true;
        }
        at = a.parent();
    }
    false
}

/// What [`read`] found for each mark, in the order of [`Marks::items`].
#[derive(Clone, Debug, Default)]
pub struct Marked {
    /// The `Marks::version` this was read for.
    version: u64,
    found: Vec<Option<Found>>,
}

#[derive(Clone, Debug)]
struct Found {
    node: NodeRef,
    size: u64,
    items: u64,
    shared_out: u64,
    scanning: bool,
}

/// Marks or unmarks the selected entry and moves the cursor down a row. The "shared between
/// entries" row, fold rows and deleted rows are not entries and cannot be marked.
pub fn toggle(app: &mut App) {
    if app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return;
    }
    let Some(index) = app.selected_index().map(|row| app.rows[row]) else {
        return;
    };
    let e = &app.snap.entries[index];
    if e.fold > 0 {
        app.status = Some("a fold row stands for many entries: → shows them".into());
        return;
    }
    if e.pseudo {
        app.status = Some("only entries can be marked".into());
        return;
    }
    if e.is_dir && crate::overlay::type_filter::active(app) {
        app.status = Some(crate::overlay::type_filter::NO_FOLDERS.into());
        return;
    }
    let node = e.node;
    let item = item_of(app, index);
    let tree = app.tree_shown().is_some();
    let marks = &mut app.marks;
    match marks
        .items
        .iter()
        .position(|it| it.target.node == Some(node) || it.target.path == item.target.path)
    {
        Some(i) => {
            marks.items.remove(i);
        }
        None => {
            if tree {
                exclusive(marks, &item.target.path);
            }
            marks.items.push(item);
        }
    }
    marks.record_change();
    app.move_by(1);
}

/// Unmarks what is inside `path` and the directories it is in, before `path` is marked in tree
/// mode.
fn exclusive(marks: &mut Marks, path: &HostPath) {
    marks
        .items
        .retain(|it| !it.target.path.starts_with(path) && !path.starts_with(&it.target.path));
}

/// The mark of entry `index` of the snapshot: a row of the current directory, or in tree mode
/// of a directory below it.
pub(crate) fn item_of(app: &App, index: usize) -> Item {
    let e = &app.snap.entries[index];
    Item {
        target: Target {
            node: Some(e.node),
            path: crate::state::tree::path_of(&app.snap, index),
            ident: Some(Ident {
                dev_ino: e.dev_ino,
                kind: e.kind,
            }),
        },
        is_dir: e.is_dir,
        size: e.size,
        items: e.items,
        shared_out: e.shared_out,
        scanning: matches!(e.mark, Mark::Scanning | Mark::Stalled | Mark::Rescanning),
        inside: false,
    }
}

/// Marks `item` or unmarks it (found by its node or path), as the cleanup view does for whole
/// groups. A read-only browser marks nothing this way.
pub fn set(app: &mut App, item: Item, on: bool) {
    if on && app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return;
    }
    let marks = &mut app.marks;
    let at = marks
        .items
        .iter()
        .position(|it| it.target.node == item.target.node || it.target.path == item.target.path);
    match (at, on) {
        (None, true) => marks.items.push(item),
        (Some(i), false) => {
            marks.items.remove(i);
        }
        _ => return,
    }
    marks.record_change();
}

/// Marks every one of `items` not marked yet, with one look-up each, and returns how many it
/// marked: for a range or a pattern, which can mark a whole directory at once. A read-only
/// browser marks nothing, as [`set`] does. In tree mode the outermost win: an item inside a
/// marked directory is left out, and marks inside a directory marked now are dropped.
pub(crate) fn add(app: &mut App, items: Vec<Item>) -> usize {
    if app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return 0;
    }
    let tree = app.tree_shown().is_some();
    let marks = &mut app.marks;
    let mut paths: HashSet<HostPath> = marks
        .items
        .iter()
        .map(|it| it.target.path.clone())
        .collect();
    let before = marks.items.len();
    for it in items {
        let known = it.target.node.is_some_and(|n| marks.nodes.contains(&n));
        if !known && paths.insert(it.target.path.clone()) {
            marks.items.push(it);
        }
    }
    let mut added = marks.items.len() - before;
    let mut dropped = 0;
    if tree && added > 0 {
        // What is inside a marked directory is left out, whether it was marked before or now.
        let paths: HashSet<&HostPath> = marks.items.iter().map(|it| &it.target.path).collect();
        let kept: Vec<bool> = marks
            .items
            .iter()
            .map(|it| !below_any(&it.target.path, &paths))
            .collect();
        added = kept[before..].iter().filter(|&&k| k).count();
        dropped = kept[..before].iter().filter(|&&k| !k).count();
        let mut keep = kept.into_iter();
        marks.items.retain(|_| keep.next().unwrap_or(true));
    }
    if added > 0 || dropped > 0 {
        marks.record_change();
    }
    added
}

pub fn clear(app: &mut App) {
    if !app.marks.is_empty() {
        app.marks.items.clear();
        app.marks.record_change();
    }
}

/// The first cell of a row: `●` when the entry is marked.
pub fn gutter(app: &App, e: &EntryData) -> Span<'static> {
    if !e.pseudo && app.marks.contains(e.node) {
        Span::styled("●", app.theme.mark)
    } else {
        Span::raw(" ")
    }
}

/// How many entries are marked and their total, for the status line.
pub fn summary(app: &App) -> Option<Span<'static>> {
    let marks = &app.marks;
    if marks.is_empty() {
        return None;
    }
    let (total, scanning) = marks.total();
    let at_least = if scanning { "at least " } else { "" };
    Some(Span::styled(
        format!(
            "● {} marked ({at_least}{})",
            marks.items.len(),
            human(total)
        ),
        app.theme.mark,
    ))
}

/// True when the marks changed since [`read`] last ran. A read ([`read`], then [`apply`]) must
/// make it false: while it is true, the run loop reads again at once.
pub fn stale(app: &App) -> bool {
    app.marks.version != app.marks.applied
}

/// Resolves the marks in the model; runs under the read lock. Each mark costs a walk up from
/// its node. The marks whose reference no longer resolves (after a rescan swap) are found by
/// path, together per parent directory: a walk down to it and one pass over its children, so
/// the cost is bounded by the children of those directories, not by marks times children.
pub fn read(m: &Model, app: &App) -> Marked {
    let tree = &m.tree;
    let items = app.marks.items();
    let rescanning = |r: NodeRef| {
        m.status.jobs.iter().any(|j| {
            j.kind == JobKind::Rescan
                && j.target == r
                && matches!(j.phase, JobPhase::Running | JobPhase::Finishing)
        })
    };
    let mut ixs: Vec<Option<Ix>> = items
        .iter()
        .map(|it| it.target.node.and_then(|r| tree.resolve_visible(r)))
        .collect();
    let mut lost: HashMap<HostPath, HashMap<&[u8], Vec<usize>>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if ixs[i].is_some() {
            continue;
        }
        let path = &it.target.path;
        match (path.parent(), path.file_name()) {
            (Some(dir), Some(name)) => lost
                .entry(dir)
                .or_default()
                .entry(name)
                .or_default()
                .push(i),
            _ => ixs[i] = it.target.resolve(tree),
        }
    }
    for (dir, mut names) in lost {
        let Some(d) = find_path(tree, &dir) else {
            for i in names.into_values().flatten() {
                ixs[i] = items[i].target.resolve(tree);
            }
            continue;
        };
        for c in tree.children(d) {
            if names.is_empty() {
                break;
            }
            for i in names.remove(tree.name(c)).into_iter().flatten() {
                ixs[i] = items[i].target.matches(tree, c).then_some(c);
            }
        }
    }
    let found = ixs
        .into_iter()
        .map(|ix| {
            let ix = ix?;
            let node = tree.node_ref(ix);
            let t = tree.total(ix);
            Some(Found {
                node,
                size: app.size_mode.of(&t),
                items: t.items,
                shared_out: t.shared_out,
                scanning: tree.dir(ix).is_some_and(|d| d.state == DirState::Pending)
                    || rescanning(node),
            })
        })
        .collect();
    Marked {
        version: app.marks.version,
        found,
    }
}

/// Takes in what [`read`] found, now in `app.extras.marked`, after every read (with a new
/// snapshot or without): updates the sizes, follows rescan swaps and drops the marks that no
/// longer resolve (deleted, or gone after a rescan).
pub fn apply(app: &mut App) {
    let (marks, marked) = (&mut app.marks, &app.extras.marked);
    if marked.version != marks.version || marked.found.len() != marks.items.len() {
        return;
    }
    marks.applied = marks.version;
    let before = marks.items.len();
    let mut swapped = false;
    let mut found = marked.found.iter();
    marks.items.retain_mut(|it| {
        let Some(Some(f)) = found.next() else {
            return false;
        };
        swapped |= it.target.node != Some(f.node);
        it.target.node = Some(f.node);
        (it.size, it.items, it.shared_out, it.scanning) =
            (f.size, f.items, f.shared_out, f.scanning);
        true
    });
    if swapped || marks.items.len() != before {
        marks.index();
    }
}

/// `*`: inverts the marks of the current directory's entries as the list shows them (after the
/// filter, without the rows tree mode opens below them), and says how many changed. The "shared
/// between entries" row is not an entry, and directories are left out while a type filter
/// shows only some of their files. A read-only browser changes nothing, as [`add`] does, and
/// the other views leave the list's rows alone, as the range mark does.
pub fn invert(app: &mut App) {
    if app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return;
    }
    if !crate::app::views::list_shown(app) {
        let list = crate::app::keys::first_key(crate::app::keys::Action::ViewFiles);
        app.status = Some(format!("marks are inverted in Files ({list})"));
        return;
    }
    let folders = !crate::overlay::type_filter::active(app);
    let rows: Vec<Item> = (app.rows.iter())
        .filter(|&&i| {
            let e = &app.snap.entries[i];
            !e.pseudo && e.depth == 0 && (folders || !e.is_dir)
        })
        .map(|&i| item_of(app, i))
        .collect();
    if rows.is_empty() {
        return;
    }
    let marks = &mut app.marks;
    let paths: HashSet<&HostPath> = marks.items.iter().map(|it| &it.target.path).collect();
    let marked = |it: &Item| {
        it.target.node.is_some_and(|n| marks.nodes.contains(&n)) || paths.contains(&it.target.path)
    };
    let (off, on): (Vec<Item>, Vec<Item>) = rows.into_iter().partition(marked);
    let gone: HashSet<&HostPath> = off.iter().map(|it| &it.target.path).collect();
    let gone_nodes: HashSet<NodeRef> = off.iter().filter_map(|it| it.target.node).collect();
    marks.items.retain(|it| {
        !gone.contains(&it.target.path) && !it.target.node.is_some_and(|n| gone_nodes.contains(&n))
    });
    let (marked, unmarked) = (on.len(), off.len());
    marks.items.extend(on);
    marks.record_change();
    app.status = Some(format!(
        "marks inverted: {marked} marked, {unmarked} unmarked"
    ));
}

#[cfg(test)]
mod tests;
