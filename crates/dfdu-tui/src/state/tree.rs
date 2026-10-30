//! Tree mode: the directories expanded below the current one, the fold threshold, and the
//! helpers the keys and the list use on the rows of a tree snapshot.
//!
//! The read ([`crate::model::snapshot::build_view`]) walks the current directory and the
//! children of every expanded directory, folding the small children of each into one row. It
//! reports what it found of the expanded set ([`Found`]), and [`TreeState::apply`] takes that
//! back, so expanded directories replaced by a rescan stay expanded.

use std::collections::{HashMap, HashSet};

use dfdu_tree::{HostPath, NodeRef};

use crate::model::snapshot::{EntryData, Ident, Snapshot, Target};

/// The most rows a read adds below the top level. A directory with more children than fit
/// shows the largest ones, and the rest in its fold row.
pub const MAX_TREE_ROWS: usize = 20_000;

/// The most directories one `*` expands.
pub const MAX_CHAIN: usize = 8;

/// The fold thresholds `z` cycles through: 1%, 0.1%, off.
pub const FOLD_STEPS: [f64; 3] = [0.01, 0.001, 0.0];

/// An expanded directory.
#[derive(Clone, Debug)]
pub struct Expanded {
    /// Where to find it again after a rescan swap.
    pub target: Target,
    /// The fold threshold for its children, when a fold row of it was expanded in place: it
    /// lowers [`TreeState::fold_ratio`] for this directory only.
    pub fold_ratio: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct TreeState {
    /// The expanded directories, by their reference as of the last read.
    pub expanded: HashMap<NodeRef, Expanded>,
    /// Children smaller than this share of their parent fold into one row; zero: none fold.
    pub fold_ratio: f64,
    /// `*` follows the largest child while it is at least this share of its parent.
    pub chain_ratio: f64,
    /// A `*` on this directory, carried out by the next read.
    pub chain: Option<Target>,
    /// Counts the changes: a snapshot read with another generation is stale ([`stale`]).
    pub generation: u64,
}

impl Default for TreeState {
    fn default() -> Self {
        TreeState {
            expanded: HashMap::new(),
            fold_ratio: FOLD_STEPS[0],
            chain_ratio: 0.10,
            chain: None,
            generation: 0,
        }
    }
}

/// What a read in tree mode found of the expanded set, for [`TreeState::apply`].
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// The [`TreeState::generation`] it was read with.
    pub generation: u64,
    /// Expanded directories found again under another reference (a rescan swap): old, new.
    pub moved: Vec<(NodeRef, NodeRef)>,
    /// Expanded directories that no longer exist, or are no longer directories.
    pub gone: Vec<NodeRef>,
    /// The directories a `*` expanded, from the top, when one was asked for.
    pub chained: Option<Vec<Target>>,
    /// Some children were left out of their rows (and counted in their fold row) because the
    /// read reached [`MAX_TREE_ROWS`].
    pub truncated: bool,
}

impl TreeState {
    pub fn is_expanded(&self, node: NodeRef) -> bool {
        self.expanded.contains_key(&node)
    }

    /// The fold threshold for the children of `node`.
    pub fn fold_ratio_of(&self, node: NodeRef) -> f64 {
        self.expanded
            .get(&node)
            .and_then(|e| e.fold_ratio)
            .map_or(self.fold_ratio, |r| r.min(self.fold_ratio))
    }

    /// Expands the directory `target` points at. Nothing happens without a reference.
    pub fn expand(&mut self, target: Target) {
        let Some(node) = target.node else {
            return;
        };
        self.expanded.entry(node).or_insert(Expanded {
            target,
            fold_ratio: None,
        });
        self.generation += 1;
    }

    /// Collapses `node`, and every directory expanded inside it, so expanding it again shows
    /// one level.
    pub fn collapse(&mut self, node: NodeRef) {
        let Some(e) = self.expanded.remove(&node) else {
            return;
        };
        let path = e.target.path;
        self.expanded
            .retain(|_, x| !x.target.path.starts_with(&path));
        self.generation += 1;
    }

    /// Expands `target`'s directory and its chain of largest children with the next read
    /// (`*`).
    pub fn expand_chain(&mut self, target: Target) {
        self.chain = Some(target);
        self.generation += 1;
    }

    /// Moves to the next fold threshold (`z`): 1% → 0.1% → off → 1%. Returns the new one.
    pub fn cycle_fold(&mut self) -> f64 {
        let at = FOLD_STEPS
            .iter()
            .position(|&s| s <= self.fold_ratio)
            .unwrap_or(0);
        self.fold_ratio = FOLD_STEPS[(at + 1) % FOLD_STEPS.len()];
        self.generation += 1;
        self.fold_ratio
    }

    /// Shows the rows folded under `parent` (`→` on its fold row): no child of `parent` folds
    /// any more, while the threshold of other directories stays as it is.
    pub fn unfold(&mut self, parent: NodeRef) {
        if let Some(e) = self.expanded.get_mut(&parent) {
            e.fold_ratio = Some(0.0);
            self.generation += 1;
        }
    }

    /// Takes back what a read found: references replaced by a rescan, directories gone, and
    /// the directories a `*` expanded.
    pub fn apply(&mut self, found: &Found) {
        for (old, new) in &found.moved {
            if let Some(mut e) = self.expanded.remove(old) {
                e.target.node = Some(*new);
                self.expanded.insert(*new, e);
            }
        }
        for node in &found.gone {
            self.expanded.remove(node);
        }
        if let Some(chained) = &found.chained {
            if found.generation == self.generation {
                self.chain = None;
            }
            for t in chained {
                if let Some(node) = t.node {
                    self.expanded.entry(node).or_insert(Expanded {
                        target: t.clone(),
                        fold_ratio: None,
                    });
                }
            }
        }
    }
}

/// Whether the snapshot was read for another tree state than the one `tree` holds now (tree
/// mode turned on or off, or a directory expanded): a new read is due.
pub fn stale(tree: Option<&TreeState>, snap: &Snapshot) -> bool {
    tree.map(|t| t.generation) != snap.tree.as_ref().map(|f| f.generation)
}

/// The parent row of entry `i` as an index into `entries`, `None` at the top.
pub fn parent(entries: &[EntryData], i: usize) -> Option<usize> {
    let p = entries[i].parent;
    (p != u32::MAX).then_some(p as usize)
}

/// Whether entry `i` is inside entry `ancestor` (below it in the tree, not equal to it).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the keys and the drawing need only the parent")
)]
pub fn inside(entries: &[EntryData], i: usize, ancestor: usize) -> bool {
    let mut cur = i;
    while let Some(p) = parent(entries, cur) {
        if p == ancestor {
            return true;
        }
        cur = p;
    }
    false
}

/// The path of entry `i` of a tree snapshot: the directory's path and the names down to it.
pub fn path_of(snap: &Snapshot, i: usize) -> HostPath {
    let mut names = vec![i];
    let mut cur = i;
    while let Some(p) = parent(&snap.entries, cur) {
        names.push(p);
        cur = p;
    }
    let mut path = snap.dir_path.clone();
    for &n in names.iter().rev() {
        path.push(&snap.entries[n].name);
    }
    path
}

/// The target of entry `i` of a tree snapshot, to pass to [`TreeState::expand`].
pub fn target_of(snap: &Snapshot, i: usize) -> Target {
    let e = &snap.entries[i];
    Target {
        node: Some(e.node),
        path: path_of(snap, i),
        ident: Some(Ident {
            dev_ino: e.dev_ino,
            kind: e.kind,
        }),
    }
}

/// Which entries are the last of their parent's children in `rows` (display order), indexed
/// by entry, for [`guide`].
pub fn last_children(entries: &[EntryData], rows: &[usize]) -> Vec<bool> {
    let mut last = vec![false; entries.len()];
    let mut seen: HashSet<u32> = HashSet::new();
    for &i in rows.iter().rev() {
        if seen.insert(entries[i].parent) {
            last[i] = true;
        }
    }
    last
}

/// The guide drawn before the name of entry `i`: nothing at the top, then for each ancestor
/// below the top `│ ` while it has siblings after it (or blank), then `├─`, or `└─` for the
/// last child. `last` comes from [`last_children`].
pub fn guide(entries: &[EntryData], last: &[bool], i: usize) -> String {
    if parent(entries, i).is_none() {
        return String::new();
    }
    let mut parts = vec![if last[i] { "└─" } else { "├─" }];
    let mut cur = i;
    while let Some(p) = parent(entries, cur) {
        if parent(entries, p).is_none() {
            break;
        }
        parts.push(if last[p] { "  " } else { "│ " });
        cur = p;
    }
    parts.iter().rev().copied().collect()
}

pub mod act;

#[cfg(test)]
pub(crate) mod tests;
