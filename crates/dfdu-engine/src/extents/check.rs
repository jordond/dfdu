//! Checks of the table against the tree, and its view of one inode, for the invariant checks
//! of tests and simulation and for debugging.

use std::collections::HashMap;
use std::fmt::Write as _;

use dfdu_tree::{Ix, NodeRef, Tree};

use super::{ExtentTable, Inode, lca};

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
    pub fn check_settled(&self, tree: &Tree) -> Result<(), String> {
        for (k, r) in &self.inodes {
            if tree.resolve_visible(r.counted_at).is_none() {
                return Err(format!(
                    "inode {k:?} is counted at a node that is not visible ({})",
                    if tree.resolve(r.counted_at).is_some() {
                        "detached"
                    } else {
                        "freed"
                    }
                ));
            }
        }
        Ok(())
    }

    /// Checks every home against a full recomputation, and every node's home bytes against
    /// the table. For the invariant checks of tests and simulation.
    pub fn check(&self, tree: &Tree) -> Result<(), String> {
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
                        let path = |ix: Ix| String::from_utf8_lossy(&tree.path(ix)).into_owned();
                        let refs: Vec<String> = seg
                            .refs
                            .iter()
                            .map(|r| match self.inodes.get(r) {
                                Some(rec) => match tree.resolve(rec.counted_at) {
                                    Some(ix) => format!(
                                        "{r:?} at {} visible {}",
                                        path(ix),
                                        tree.resolve_visible(rec.counted_at).is_some()
                                    ),
                                    None => format!("{r:?} at a freed node"),
                                },
                                None => format!("{r:?} not in the table"),
                            })
                            .collect();
                        return Err(format!(
                            "segment {s}..{}: home {}, lowest common ancestor {:?}; refs {refs:?}",
                            seg.end,
                            path(h),
                            full.map(path)
                        ));
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
                return Err(format!(
                    "{}: home {} bytes, table {bytes}",
                    String::from_utf8_lossy(&tree.path(ix)),
                    tree.shared_here(ix)
                ));
            }
        }
        Ok(())
    }
}
