//! What the details pane shows of the selected entry, read under the read lock ([`facts`]): its
//! facts, its largest children, and the rule it matched or the matches below it by category.
//! While the pane is open ([`Details`]) each read reads it for the entry selected ([`subject`]):
//! the cleanup view's item while that view is shown, else the list's entry ([`read`]).

use dfdu_engine::{Model, SizeMode, subvol_label};
use dfdu_tree::{HostPath, Ix, Kind, NodeFlags, NodeRef, RuleIx, Safety, Totals, Tree};

use crate::snapshot::Mark;
use crate::state::SessionState;

/// Children listed as the largest.
const LARGEST: usize = 5;
/// Directories with more children than this get no list of the largest: finding them reads
/// every child under the read lock.
pub const LARGEST_MAX_CHILDREN: usize = 100_000;

/// What the details read found for the selected entry under the read lock.
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
    pub flags: dfdu_tree::ClassFlags,
}

/// The rule node `ix` matched, or the match above it.
pub fn matched(m: &Model, ix: Ix) -> Option<Matched> {
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
pub fn categories(m: &Model, ix: Ix, mode: SizeMode) -> Vec<(&'static str, usize, u64, u64)> {
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
#[derive(Copy, Clone, Debug)]
pub struct Subject {
    pub node: NodeRef,
    /// The "shared between entries" row, which is not a node of its own.
    pub pseudo: bool,
    pub compressed: bool,
    pub mark: Mark,
}

/// The facts of `e` in size mode `mode`, listing the largest children only of directories with
/// at most `MAX` children; `None` when it is not a node of its own (the "shared between entries"
/// row, or a deleted entry). Costs a walk up to the root for the path and, for a directory, a
/// look at each child; never a walk of the subtree.
pub fn facts<const MAX: usize>(m: &Model, e: &Subject, mode: SizeMode) -> Option<Facts> {
    let tree = &m.tree;
    match tree.resolve_visible(e.node) {
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
            largest: largest::<MAX>(tree, ix, mode),
            size_mode: mode,
            matched: matched(m, ix),
            categories: if tree.dir(ix).is_some() {
                categories(m, ix, mode)
            } else {
                Vec::new()
            },
        }),
        _ => None,
    }
}

/// The largest children of `ix`, or how many there are when more than `MAX`.
pub fn largest<const MAX: usize>(tree: &Tree, ix: Ix, mode: SizeMode) -> Largest {
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

/// Whether the pane is open, and what it was last read for.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub open: bool,
    /// What `s.extras.details` was read for.
    read_for: ReadFor,
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

/// Opens or closes the pane.
pub fn toggle(s: &mut SessionState) {
    s.details = Details {
        open: !s.details.open,
        ..Details::default()
    };
}

/// The cleanup view's selected item while that view is shown, else the list's selected entry.
pub fn subject(s: &SessionState) -> Option<Subject> {
    if crate::cleanup::active(s) {
        let it = crate::cleanup::selected(s)?;
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
    s.selected_entry().map(|e| Subject {
        node: e.node,
        pseudo: e.pseudo,
        compressed: e.compressed,
        mark: e.mark,
    })
}

/// Reads the selected entry's details; runs under the read lock. Costs a walk up to the root
/// for the path and, for a directory of at most [`LARGEST_MAX_CHILDREN`] children, a look at
/// each child; never a walk of the subtree.
pub fn read(m: &Model, s: &SessionState) -> Info {
    read_within::<LARGEST_MAX_CHILDREN>(m, s)
}

/// [`read`], listing the largest children only of directories with at most `MAX` children.
pub fn read_within<const MAX: usize>(m: &Model, s: &SessionState) -> Info {
    if !s.details.open {
        return Info::default();
    }
    let Some(e) = subject(s) else {
        return Info::default();
    };
    Info {
        node: Some(e.node),
        facts: facts::<MAX>(m, &e, s.size_mode),
    }
}

/// True when the pane shows something other than what [`read`] last read. A read ([`read`],
/// then [`apply`]) must make it false: while it is true, the front end reads again at once.
pub fn stale(s: &SessionState) -> bool {
    s.details.open && s.details.read_for != ReadFor::Entry(subject(s).map(|e| e.node))
}

/// Takes in what [`read`] found, now in `s.extras.details`, after every read (with a new
/// snapshot or without); true when it is of another entry than the last, which the pane shows
/// from its top.
///
/// [`read`] runs before the refresh takes in the snapshot read with it, so it saw the previous
/// snapshot's row: the mark and the compressed flag are taken again from the row the list now
/// draws for the same entry.
pub fn apply(s: &mut SessionState) -> bool {
    let node = s.extras.details.node;
    let other = s.details.read_for != ReadFor::Entry(node);
    s.details.read_for = ReadFor::Entry(node);
    let row = subject(s)
        .filter(|e| Some(e.node) == node)
        .map(|e| (e.mark, e.compressed));
    if let (Some((mark, compressed)), Some(f)) = (row, &mut s.extras.details.facts) {
        f.mark = mark;
        f.compressed = compressed;
    }
    other
}

#[cfg(test)]
mod tests;
