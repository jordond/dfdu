//! Errors met during the scan: what went wrong with an entry, and the list of all of them
//! (the browser's `!` list and `dfdu scan --errors`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use dfdu_tree::{DirState, ErrKind, Ix, NodeRef, Tree};

/// What went wrong reading this entry, if anything ("permission denied", "partly read:
/// input/output error").
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
pub fn errors_below(tree: &Tree, ix: Ix) -> u64 {
    tree.total(ix).errors - tree.error(ix).is_some() as u64
}

#[derive(Clone, Debug)]
pub struct ErrorItem {
    pub node: NodeRef,
    pub path: PathBuf,
    pub is_dir: bool,
    pub problem: String,
}

#[derive(Clone, Debug, Default)]
pub struct ErrorList {
    /// The directory the list covers.
    pub root: PathBuf,
    /// The first errors in path order, at most the limit given to [`errors`].
    pub items: Vec<ErrorItem>,
    /// All errors below the root, listed or not.
    pub total: u64,
    /// How many of each kind, most common first.
    pub kinds: Vec<(ErrKind, u64)>,
}

impl ErrorList {
    /// "2 permission denied, 1 input/output error".
    pub fn summary(&self) -> String {
        self.kinds
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A hint for permission errors when not running as root.
    pub fn hint(&self) -> Option<&'static str> {
        let denied = self
            .kinds
            .iter()
            .any(|(k, _)| *k == ErrKind::PermissionDenied);
        (denied && !dfdu_sys::is_privileged()).then_some(if cfg!(windows) {
            "run as administrator to read them"
        } else {
            "run as root (sudo) to read them"
        })
    }
}

/// The errors in `root`'s subtree, in path order, listing at most `limit`. Only subtrees that
/// contain errors are visited.
pub fn errors(tree: &Tree, root: Ix, limit: usize) -> ErrorList {
    let mut list = ErrorList {
        root: tree.path_buf(root),
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
                    path: tree.path_buf(ix),
                    is_dir: tree.dir(ix).is_some(),
                    problem: problem(tree, ix).unwrap_or_default(),
                });
            }
        }
        let mut kids: Vec<Ix> = tree
            .children(ix)
            .filter(|&c| tree.total(c).errors > 0)
            .collect();
        // Reverse name order on the stack pops in name order.
        kids.sort_by(|&a, &b| tree.name(b).cmp(tree.name(a)));
        stack.extend(kids);
    }
    list.kinds = kinds.into_iter().collect();
    list.kinds.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    list
}
