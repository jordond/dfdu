//! Marked entries: `Space` marks, the footer shows their live total, and a delete acts on them.
//! `v` marks a range as the cursor moves ([`range`]); the marks pane lists them ([`pane`]).
//!
//! In the list a marked directory can hold marked entries, which count and go with it. In tree
//! mode, where both show at once, a directory and something inside it are never both marked:
//! marking one unmarks the other (`exclusive`), as marking a container in the cleanup view
//! marks only its rest.
//!
//! A read-only browser marks nothing, as nothing marked could be deleted: every way of marking
//! says so on the status line instead.
//!
//! The front end draws the marks from what this keeps ([`is_marked`], [`summary`]), and says
//! whether it draws the list's rows, where the range mark and inverting the marks work. Whether
//! the rows are a tree comes from the view shown ([`SessionState::tree_shown`]).

use std::collections::{HashMap, HashSet};

use dfdu_engine::{JobKind, JobPhase, Model};
use dfdu_tree::{DirState, HostPath, Ident, Ix, NodeRef, Target, find_path};

use crate::actions::{Action, first_key};
use crate::format::human;
use crate::snapshot::{EntryData, Mark};
use crate::state::{NO_FOLDERS, READ_ONLY, SessionState};

pub mod pane;
pub mod range;

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
pub fn toggle(s: &mut SessionState) {
    let tree = s.tree_shown().is_some();
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return;
    }
    let Some(index) = s.selected_index().map(|row| s.list.rows[row]) else {
        return;
    };
    let e = &s.snap.entries[index];
    if e.fold > 0 {
        s.status = Some("a fold row stands for many entries: → shows them".into());
        return;
    }
    if e.pseudo {
        s.status = Some("only entries can be marked".into());
        return;
    }
    if e.is_dir && s.types_active() {
        s.status = Some(NO_FOLDERS.into());
        return;
    }
    let node = e.node;
    let item = item_of(s, index);
    let marks = &mut s.marks;
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
    s.move_by(1);
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
pub fn item_of(s: &SessionState, index: usize) -> Item {
    let e = &s.snap.entries[index];
    Item {
        target: Target {
            node: Some(e.node),
            path: crate::tree::path_of(&s.snap, index),
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
pub fn set(s: &mut SessionState, item: Item, on: bool) {
    if on && s.read_only {
        s.status = Some(READ_ONLY.into());
        return;
    }
    let marks = &mut s.marks;
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
/// browser marks nothing, as [`set`] does. While the rows are a tree the outermost win: an item
/// inside a marked directory is left out, and marks inside a directory marked now are dropped.
pub fn add(s: &mut SessionState, items: Vec<Item>) -> usize {
    let tree = s.tree_shown().is_some();
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return 0;
    }
    let marks = &mut s.marks;
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

pub fn clear(s: &mut SessionState) {
    if !s.marks.is_empty() {
        s.marks.items.clear();
        s.marks.record_change();
    }
}

/// True when the row of `e` shows as marked. The "shared between entries" row has its
/// directory's reference, and never does.
pub fn is_marked(s: &SessionState, e: &EntryData) -> bool {
    !e.pseudo && s.marks.contains(e.node)
}

/// How many entries are marked and their total, for the status line; `None` when nothing is.
pub fn summary(s: &SessionState) -> Option<String> {
    let marks = &s.marks;
    if marks.is_empty() {
        return None;
    }
    let (total, scanning) = marks.total();
    let at_least = if scanning { "at least " } else { "" };
    Some(format!(
        "● {} marked ({at_least}{})",
        marks.items.len(),
        human(total)
    ))
}

/// True when the marks changed since [`read`] last ran. A read ([`read`], then [`apply`]) must
/// make it false: while it is true, the run loop reads again at once.
pub fn stale(s: &SessionState) -> bool {
    s.marks.version != s.marks.applied
}

/// Resolves the marks in the model; runs under the read lock. Each mark costs a walk up from
/// its node. The marks whose reference no longer resolves (after a rescan swap) are found by
/// path, together per parent directory: a walk down to it and one pass over its children, so
/// the cost is bounded by the children of those directories, not by marks times children.
pub fn read(m: &Model, s: &SessionState) -> Marked {
    let tree = &m.tree;
    let items = s.marks.items();
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
                size: s.size_mode.of(&t),
                items: t.items,
                shared_out: t.shared_out,
                scanning: tree.dir(ix).is_some_and(|d| d.state == DirState::Pending)
                    || rescanning(node),
            })
        })
        .collect();
    Marked {
        version: s.marks.version,
        found,
    }
}

/// Takes in what [`read`] found after every read (with a new snapshot or without): updates the
/// sizes, follows rescan swaps and drops the marks that no longer resolve (deleted, or gone
/// after a rescan).
pub fn apply(s: &mut SessionState, marked: &Marked) {
    let marks = &mut s.marks;
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
/// the other views leave the list's rows alone, as the range mark does. `list_shown`: the
/// list's rows are drawn.
pub fn invert(s: &mut SessionState, list_shown: bool) {
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return;
    }
    if !list_shown {
        let list = first_key(Action::ViewFiles);
        s.status = Some(format!("marks are inverted in Files ({list})"));
        return;
    }
    let folders = !s.types_active();
    let rows: Vec<Item> = (s.list.rows.iter())
        .filter(|&&i| {
            let e = &s.snap.entries[i];
            !e.pseudo && e.depth == 0 && (folders || !e.is_dir)
        })
        .map(|&i| item_of(s, i))
        .collect();
    if rows.is_empty() {
        return;
    }
    let marks = &mut s.marks;
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
    s.status = Some(format!(
        "marks inverted: {marked} marked, {unmarked} unmarked"
    ));
}

#[cfg(test)]
mod tests;
