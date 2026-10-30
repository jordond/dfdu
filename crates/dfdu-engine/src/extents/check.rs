//! Checks of the table against the tree, and its view of one inode, for the invariant checks
//! of tests and simulation and for debugging.

use std::collections::HashMap;
use std::fmt::{self, Write as _};

use dfdu_tree::{Ix, NodeRef, Tree};

use super::{ExtentTable, Inode, lca};

/// A way the table disagrees with the tree, found by its checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// A measured inode is counted at a node that is not visible: one in a rescan's shadow, or
    /// one `gone` (detached or freed).
    NotVisible { inode: Inode, gone: bool },
    /// The home of segment `start..end` is not the lowest common ancestor of its references,
    /// though none of them is out of sight: the paths of both, and where each reference is
    /// counted.
    Home {
        start: u64,
        end: u64,
        home: String,
        lca: Option<String>,
        refs: Vec<String>,
    },
    /// The shared bytes the tree homes at `path` are not the table's.
    Bytes { path: String, tree: u64, table: u64 },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::NotVisible { inode, gone } => write!(
                f,
                "inode {inode:?} is counted at a node that is not visible ({})",
                if *gone {
                    "detached or freed"
                } else {
                    "in a shadow"
                }
            ),
            Mismatch::Home {
                start,
                end,
                home,
                lca,
                refs,
            } => write!(
                f,
                "segment {start}..{end}: home {home}, lowest common ancestor {lca:?}; refs \
                 {refs:?}"
            ),
            Mismatch::Bytes { path, tree, table } => {
                write!(f, "{path}: home {tree} bytes, table {table}")
            }
        }
    }
}

impl std::error::Error for Mismatch {}

impl ExtentTable {
    /// The table's view of one inode, for debugging.
    pub fn describe(&self, tree: &Tree, inode: Inode) -> String {
        let Some(rec) = self.inodes.get(&inode) else {
            return format!("{inode:?}: not in the table");
        };
        let path = |r: NodeRef| {
            tree.resolve(r).map_or_else(
                || "freed".into(),
                |ix| String::from_utf8_lossy(&tree.path(ix)).into_owned(),
            )
        };
        let mut out = format!(
            "{inode:?} at {} excl {} ranges {:?} points {:?}",
            path(rec.counted_at),
            rec.excl,
            rec.ranges,
            rec.points
        );
        for &(a, b) in &rec.ranges {
            for (&(_, st), seg) in self.segs.range((rec.fs, a)..(rec.fs, b)) {
                let _ = write!(
                    out,
                    "\n  seg {st}..{} refs {:?} home {:?} out {}",
                    seg.end,
                    seg.refs,
                    seg.home.map(path),
                    seg.out
                );
            }
        }
        out
    }

    /// Checks that every measured inode is counted at a visible node.
    pub fn check_settled(&self, tree: &Tree) -> Result<(), Mismatch> {
        for (&inode, r) in &self.inodes {
            if tree.resolve_visible(r.counted_at).is_none() {
                return Err(Mismatch::NotVisible {
                    inode,
                    gone: tree.resolve(r.counted_at).is_none(),
                });
            }
        }
        Ok(())
    }

    /// Checks every home against a full recomputation, and every node's home bytes against
    /// the table. For the invariant checks of tests and simulation.
    pub fn check(&self, tree: &Tree) -> Result<(), Mismatch> {
        let path = |ix: Ix| String::from_utf8_lossy(&tree.path(ix)).into_owned();
        let mut expect: HashMap<Ix, u64> = HashMap::new();
        for (&(_, s), seg) in &self.segs {
            if let Some(h) = seg.home.and_then(|h| tree.resolve_visible(h)) {
                let full = lca(tree, &self.inodes, seg.refs.iter().copied());
                if full != Some(h) {
                    // Allowed only while a reference is not visible: its subtree is detached and
                    // not freed yet, or its hard link was freed and the re-election that moves
                    // it to another link is queued. Both end in a full recomputation.
                    let stale = seg.refs.iter().any(|r| {
                        self.inodes
                            .get(r)
                            .is_some_and(|rec| tree.resolve_visible(rec.counted_at).is_none())
                    });
                    if !stale {
                        let refs = seg.refs.iter().map(|r| match self.inodes.get(r) {
                            Some(rec) => match tree.resolve(rec.counted_at) {
                                Some(ix) => format!(
                                    "{r:?} at {} visible {}",
                                    path(ix),
                                    tree.resolve_visible(rec.counted_at).is_some()
                                ),
                                None => format!("{r:?} at a freed node"),
                            },
                            None => format!("{r:?} not in the table"),
                        });
                        return Err(Mismatch::Home {
                            start: s,
                            end: seg.end,
                            home: path(h),
                            lca: full.map(path),
                            refs: refs.collect(),
                        });
                    }
                }
                *expect.entry(h).or_default() += seg.end - s;
            }
        }
        for pt in self.points.values() {
            if let Some(h) = pt.home.and_then(|h| tree.resolve_visible(h)) {
                *expect.entry(h).or_default() += pt.charged;
            }
        }
        for (&ix, &bytes) in &expect {
            if tree.shared_here(ix) != bytes {
                return Err(Mismatch::Bytes {
                    path: path(ix),
                    tree: tree.shared_here(ix),
                    table: bytes,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
