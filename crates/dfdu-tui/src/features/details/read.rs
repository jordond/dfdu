//! What the pane shows of the selected entry, read under the read lock ([`read`]): its facts,
//! its largest children, and the rule it matched or the matches below it by category.

use dfdu_engine::{Model, SizeMode, subvol_label};
use dfdu_tree::{HostPath, Ix, Kind, NodeFlags, NodeRef, RuleIx, Safety, Totals, Tree};

use crate::app::App;
use crate::model::snapshot::Mark;

/// Children listed as the largest.
const LARGEST: usize = 5;
/// Directories with more children than this get no list of the largest: finding them reads
/// every child under the read lock.
const LARGEST_MAX_CHILDREN: usize = 100_000;

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
    pub path: HostPath,
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
    pub above: Option<HostPath>,
    pub explanation: String,
    pub lost: String,
    /// The `degrade` condition that holds.
    pub degraded: Option<String>,
    pub flags: u8,
}

/// The rule node `ix` matched, or the match above it.
pub(super) fn matched(m: &Model, ix: Ix) -> Option<Matched> {
    let rules = m.rules.as_ref()?;
    let (at, c) = rules.class_in_effect(&m.tree, ix)?;
    let r = rules.rule(c.rule);
    let cat = r.category();
    Some(Matched {
        rule: r.name.clone(),
        category: cat.name,
        slot: cat.color as usize,
        safety: c.safety,
        above: (at != ix).then(|| m.tree.host_path(at)),
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

/// What the pane describes.
pub(super) struct Subject {
    pub(super) node: NodeRef,
    pseudo: bool,
    pub(super) compressed: bool,
    pub(super) mark: Mark,
}

/// The cleanup view's selected item while that view is shown, else the list's selected entry.
pub(super) fn subject(app: &App) -> Option<Subject> {
    if crate::features::cleanup::active(app) {
        let it = crate::features::cleanup::selected(app)?;
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
pub(super) fn read_within<const MAX: usize>(m: &Model, app: &App) -> Info {
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
            path: tree.host_path(ix),
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

#[cfg(test)]
mod tests;
