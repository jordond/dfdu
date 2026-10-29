//! The rows of tree mode: the children of every expanded directory, read after the current
//! directory's own entries. This runs under the read lock, so it is bounded by
//! [`MAX_TREE_ROWS`]: children are read only for expanded directories, and a directory with
//! more children than fit gives its largest ones, picked by a partial select.

use std::collections::{HashMap, HashSet, VecDeque};

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{Ix, Kind, NodeRef, Totals};

use super::{EntryData, Mark, Snapshot, Tag, Target, class_below, entry_of, mark_of};
use crate::state::tree::{Found, MAX_CHAIN, MAX_TREE_ROWS, TreeState};

/// A directory whose children get rows: its row, node and class in effect.
struct Open {
    row: usize,
    ix: Ix,
    class: Option<(Ix, dfdu_tree::Class)>,
}

/// What the walk needs of the read: the model, the size mode and type filter of the rows, and
/// the directories being rescanned.
pub(super) struct Read<'a> {
    pub m: &'a Model,
    pub mode: SizeMode,
    pub types: &'a TypeFilter,
    pub rescanning: &'a HashSet<NodeRef>,
    /// The class in effect at the current directory.
    pub dir_class: Option<(Ix, dfdu_tree::Class)>,
}

impl Read<'_> {
    /// The totals of node `c` as its row counts them: only the files the type filter matches,
    /// `None` when it has none.
    fn totals(&self, c: Ix) -> Option<Totals> {
        if self.types.is_empty() {
            return Some(self.m.tree.total(c));
        }
        let t = filetype::sum(&self.m.tree, c, self.types, None);
        (t.items > 0).then_some(t)
    }
}

/// Adds the rows below the expanded directories to `snap`, whose entries are the current
/// directory's, and records in `snap.tree` what it found of the expanded set.
pub(super) fn expand(r: &Read<'_>, state: &TreeState, snap: &mut Snapshot) {
    let tree = &r.m.tree;
    let mut found = Found {
        generation: state.generation,
        ..Found::default()
    };
    // Expanded directories by their reference now, with their fold threshold.
    let mut open: HashMap<NodeRef, f64> = HashMap::with_capacity(state.expanded.len());
    for (&key, e) in &state.expanded {
        match e.target.resolve(tree).filter(|&ix| tree.dir(ix).is_some()) {
            Some(ix) => {
                let now = tree.node_ref(ix);
                if now != key {
                    found.moved.push((key, now));
                }
                open.insert(now, state.fold_ratio_of(key));
            }
            None => found.gone.push(key),
        }
    }
    if let Some(t) = &state.chain {
        let chained = t.resolve(tree).map_or_else(Vec::new, |ix| {
            chain(r, ix, state.chain_ratio)
                .into_iter()
                .map(|c| Target::of(tree, c))
                .collect()
        });
        for t in &chained {
            if let Some(node) = t.node {
                open.entry(node).or_insert(state.fold_ratio);
            }
        }
        found.chained = Some(chained);
    }
    if open.is_empty() {
        snap.tree = Some(found);
        return;
    }

    let mut queue: VecDeque<Open> = VecDeque::new();
    for (row, e) in snap.entries.iter().enumerate() {
        if e.is_dir
            && !e.pseudo
            && open.contains_key(&e.node)
            && let Some(ix) = tree.resolve_visible(e.node)
        {
            queue.push_back(Open {
                row,
                ix,
                class: class_below(r.m, r.dir_class, ix),
            });
        }
    }
    let mut budget = MAX_TREE_ROWS;
    while let Some(o) = queue.pop_front() {
        let parent = &snap.entries[o.row];
        let ratio = open[&parent.node];
        let depth = parent.depth;
        if depth == u8::MAX {
            continue;
        }
        let shown = children(r, &o, parent.size, ratio, &mut budget, &mut found);
        let row = u32::try_from(o.row).unwrap_or(u32::MAX);
        for (c, t) in shown.kept {
            let mut e = entry_of(r.m, c, o.class, r.rescanning, r.mode);
            e.size = r.mode.of(&t);
            e.items = t.items;
            e.shared_out = t.shared_out;
            e.depth = depth + 1;
            e.parent = row;
            if e.is_dir && open.contains_key(&e.node) {
                queue.push_back(Open {
                    row: snap.entries.len(),
                    ix: c,
                    class: class_below(r.m, o.class, c),
                });
            }
            snap.entries.push(e);
        }
        if let Some(mut f) = shown.fold {
            f.depth = depth + 1;
            f.parent = row;
            f.tag = o.class.map(|(_, class)| Tag {
                class,
                inherited: true,
            });
            snap.entries.push(f);
        }
    }
    snap.tree = Some(found);
}

/// The children of an open directory: those with rows, and the fold row of the rest.
struct Shown {
    kept: Vec<(Ix, Totals)>,
    fold: Option<EntryData>,
}

/// Splits the children of `o` into the ones at least `ratio` of `size` (its row's size),
/// as many as `budget` has left, largest first; the others go to the fold row.
fn children(
    r: &Read<'_>,
    o: &Open,
    size: u64,
    ratio: f64,
    budget: &mut usize,
    found: &mut Found,
) -> Shown {
    let tree = &r.m.tree;
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a threshold in bytes; being off by a few bytes on a size of exabytes does not matter"
    )]
    let threshold = (size as f64 * ratio).ceil() as u64;
    let mut kept: Vec<(u64, Ix, Totals)> = Vec::new();
    let mut folded: Vec<(u64, Ix, Totals)> = Vec::new();
    for c in tree.children(o.ix) {
        let Some(t) = r.totals(c) else {
            continue;
        };
        let s = r.mode.of(&t);
        if s >= threshold {
            kept.push((s, c, t));
        } else {
            folded.push((s, c, t));
        }
    }
    if kept.len() > *budget {
        // Only the largest fit: a partial select, not a sort of a directory that may have a
        // million entries.
        kept.select_nth_unstable_by(*budget, |a, b| b.0.cmp(&a.0));
        folded.extend(kept.drain(*budget..));
        found.truncated = true;
    }
    *budget -= kept.len();
    Shown {
        kept: kept.into_iter().map(|(_, c, t)| (c, t)).collect(),
        fold: fold_row(r, &folded),
    }
}

/// The row standing for the `folded` children: their count, summed sizes, and the most
/// severe mark among them. Its reference is the largest one's, so the selection stays on it
/// when the fold row is expanded in place.
fn fold_row(r: &Read<'_>, folded: &[(u64, Ix, Totals)]) -> Option<EntryData> {
    let tree = &r.m.tree;
    let largest = folded.iter().max_by_key(|f| f.0)?;
    let mut sum = Totals::default();
    let mut mark = Mark::None;
    let mut compressed = false;
    let mut mtime = i64::MIN;
    for &(_, c, ref t) in folded {
        sum.add(t);
        let m = mark_of(tree, c, r.rescanning);
        if severity(m) > severity(mark) {
            mark = m;
        }
        compressed |= tree.flags(c).has(dfdu_tree::NodeFlags::COMPRESSED);
        mtime = mtime.max(tree.mtime(c));
    }
    let n = u32::try_from(folded.len()).unwrap_or(u32::MAX);
    Some(EntryData {
        node: tree.node_ref(largest.1),
        name: format!("… {n} more").as_bytes().into(),
        kind: Kind::Other,
        is_dir: false,
        size: r.mode.of(&sum),
        items: sum.items,
        mtime,
        dev_ino: (0, 0),
        mark,
        error: None,
        problem: None,
        errors_inside: sum.errors,
        label: None,
        tag: None,
        shared_out: sum.shared_out,
        compressed,
        pseudo: true,
        ghost: false,
        depth: 0,
        parent: u32::MAX,
        fold: n,
    })
}

/// How much a mark weighs when one row stands for many: problems first, then work in
/// progress, then notes about where the data lives.
fn severity(m: Mark) -> u8 {
    match m {
        Mark::None => 0,
        Mark::OtherFs | Mark::DupMount | Mark::Network | Mark::Kernel => 1,
        Mark::Scanning => 2,
        Mark::Rescanning => 3,
        Mark::Deleting => 4,
        Mark::ErrorsInside => 5,
        Mark::Incomplete => 6,
        Mark::Stalled => 7,
        Mark::Error => 8,
    }
}

/// `start` and its chain of largest children (`*`): the largest child is followed while it is
/// a directory of at least `ratio` of its parent, for [`MAX_CHAIN`] directories at most.
pub(super) fn chain(r: &Read<'_>, start: Ix, ratio: f64) -> Vec<Ix> {
    let tree = &r.m.tree;
    let mut out = vec![start];
    let mut cur = start;
    while out.len() < MAX_CHAIN {
        let size = r.mode.of(&tree.total(cur));
        let Some((s, c)) = tree
            .children(cur)
            .map(|c| (r.mode.of(&tree.total(c)), c))
            .max_by_key(|&(s, _)| s)
        else {
            break;
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "comparing shares; the precision of f64 is plenty"
        )]
        let enough = s > 0 && s as f64 >= ratio * size as f64;
        if !enough || tree.dir(c).is_none() {
            break;
        }
        out.push(c);
        cur = c;
    }
    out
}
