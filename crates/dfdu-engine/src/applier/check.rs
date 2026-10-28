//! Invariant checks for tests and the simulation harness.

use dfdu_scan::Backend;

use super::{Applier, JobPhase, Model};
use crate::extents::Inode;

impl<B: Backend> Applier<B> {
    /// The extent table's view of one inode, for debugging.
    pub fn describe_extents(&self, m: &Model, inode: Inode) -> String {
        self.extents.describe(&m.tree, inode)
    }

    /// Checks what must hold once nothing is pending: every measured inode is counted at a
    /// visible node.
    pub fn check_settled(&self, m: &Model) -> Result<(), String> {
        self.extents.check_settled(&m.tree)
    }

    /// Checks the tree's structure and totals, and the pending counts of running jobs. Linear
    /// in tree size.
    pub fn check(&self, m: &Model) -> Result<(), String> {
        m.tree
            .check(&self.shadow_roots(&m.tree))
            .map_err(|v| format!("{v:?}"))?;
        self.extents.check(&m.tree)?;
        for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
            let Some(root) = m.tree.resolve(j.root) else {
                continue;
            };
            let mut stack = vec![root];
            while let Some(ix) = stack.pop() {
                let rec = m.tree.dir(ix).expect("dir");
                if rec.job != j.id.0 || rec.cut {
                    continue;
                }
                let mut expect = (!rec.listing_closed) as u32;
                for c in m.tree.children(ix) {
                    if let Some(cr) = m.tree.dir(c) {
                        if cr.job == j.id.0 && !cr.reported {
                            expect += 1;
                        }
                        stack.push(c);
                    }
                }
                if rec.pending != expect {
                    return Err(format!(
                        "{:?} pending {} expected {} (job {})",
                        m.tree.path_buf(ix).display(),
                        rec.pending,
                        expect,
                        j.id.0
                    ));
                }
                if (rec.pending == 0) != rec.state.is_terminal() {
                    return Err(format!(
                        "pending {} with state {:?}",
                        rec.pending, rec.state
                    ));
                }
            }
        }
        Ok(())
    }
}
