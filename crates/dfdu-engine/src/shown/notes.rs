//! What the browser and the headless commands say about an entry besides its size: errors met
//! during the scan (what went wrong with an entry, and the list of all of them for the
//! browser's `!` list and `dfdu scan --errors`), and btrfs subvolume labels.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use dfdu_tree::{DirState, ErrKind, HostPath, Ix, NodeRef, Tree};

use super::sizes::human;
use crate::Model;

/// What went wrong reading this entry, if anything ("permission denied", "partly read:
/// input/output error").
#[must_use]
pub fn problem(tree: &Tree, ix: Ix) -> Option<String> {
    let err = tree.error(ix)?;
    Some(match tree.dir(ix).map(|d| d.state) {
        Some(DirState::Partial) => format!("partly read: {err}"),
        Some(DirState::Abandoned) => "not responding: skipped".into(),
        Some(DirState::Lost) => "scan lost: rescan it".into(),
        _ => err.to_string(),
    })
}

/// Errors below the entry, not counting its own.
#[must_use]
pub fn errors_below(tree: &Tree, ix: Ix) -> u64 {
    tree.total(ix).errors - u64::from(tree.error(ix).is_some())
}

#[derive(Clone, Debug)]
pub struct ErrorItem {
    pub node: NodeRef,
    pub path: HostPath,
    pub is_dir: bool,
    pub problem: String,
}

#[derive(Clone, Debug, Default)]
pub struct ErrorList {
    /// The directory the list covers.
    pub root: HostPath,
    /// The first errors in path order, at most the limit given to [`errors`].
    pub items: Vec<ErrorItem>,
    /// All errors below the root, listed or not.
    pub total: u64,
    /// How many of each kind, most common first.
    pub kinds: Vec<(ErrKind, u64)>,
}

impl ErrorList {
    /// "2 permission denied, 1 input/output error".
    #[must_use]
    pub fn summary(&self) -> String {
        self.kinds
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A hint for permission errors when not running as root (as administrator on Windows).
    #[must_use]
    pub fn hint(&self) -> Option<&'static str> {
        // Only asks whether the process is privileged when there is something to hint at.
        self.hint_as(self.denied() && dfdu_sys::is_privileged(), cfg!(windows))
    }

    /// [`ErrorList::hint`] for a user who is `privileged` or not, worded for Windows or not.
    #[must_use]
    pub fn hint_as(&self, privileged: bool, windows: bool) -> Option<&'static str> {
        (self.denied() && !privileged).then_some(if windows {
            "run as administrator to read them"
        } else {
            "run as root (sudo) to read them"
        })
    }

    fn denied(&self) -> bool {
        self.kinds
            .iter()
            .any(|(k, _)| *k == ErrKind::PermissionDenied)
    }
}

/// The errors in `root`'s subtree, in path order, listing at most `limit`. Only subtrees that
/// contain errors are visited.
#[must_use]
pub fn errors(tree: &Tree, root: Ix, limit: usize) -> ErrorList {
    let mut list = ErrorList {
        root: tree.host_path(root),
        ..Default::default()
    };
    let mut kinds: BTreeMap<ErrKind, u64> = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        if let Some(e) = tree.error(ix) {
            list.total += 1;
            *kinds.entry(e).or_default() += 1;
            if list.items.len() < limit {
                list.items.push(ErrorItem {
                    node: tree.node_ref(ix),
                    path: tree.host_path(ix),
                    is_dir: tree.dir(ix).is_some(),
                    problem: problem(tree, ix).unwrap_or_default(),
                });
            }
        }
        let mut children: Vec<Ix> = tree
            .children(ix)
            .filter(|&c| tree.total(c).errors > 0)
            .collect();
        // Reverse name order on the stack pops in name order.
        children.sort_by(|&a, &b| tree.name(b).cmp(tree.name(a)));
        stack.extend(children);
    }
    list.kinds = kinds.into_iter().collect();
    list.kinds.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    list
}

/// "subvolume" or "snapshot of NAME", with ", read-only", for btrfs subvolume roots.
#[must_use]
pub fn subvol_label(m: &Model, ix: Ix) -> Option<String> {
    let tree = &m.tree;
    let sv = m.subvols.get(&tree.node_ref(ix))?;
    let mut s = match sv.parent_uuid {
        Some(origin) => match m
            .subvols
            .iter()
            .find(|(_, o)| o.uuid == origin)
            .and_then(|(r, _)| tree.resolve_visible(*r))
        {
            Some(o) => format!("snapshot of {}", String::from_utf8_lossy(tree.name(o))),
            None => "snapshot".into(),
        },
        None if sv.id == 5 => "btrfs top level".into(),
        None => "subvolume".into(),
    };
    if sv.read_only {
        s.push_str(", read-only");
    }
    if let Some((rfer, excl)) = sv.qgroup {
        let _ = write!(
            s,
            "; quota: {} referenced, {} exclusive",
            human(rfer),
            human(excl)
        );
    }
    Some(s)
}
