//! Plans whose items can still be put back, and rescans of what an undo or a native command
//! changed.

use dfdu_clean::Mode;
use dfdu_scan::Backend;
use dfdu_tree::{HostPath, Ix, Kind, Tree};

use super::{Applier, Count, Model, UndoInfo};

#[cfg(test)]
mod tests;

/// What of a plan can still be put back: the mode it deleted with, and those items.
pub(super) struct Undoable {
    mode: Mode,
    count: Count,
}

impl<B: Backend> Applier<B> {
    /// An item of `plan`, deleted with `mode`, can be put back.
    pub(super) fn add_undoable(&mut self, plan: u64, mode: Mode, bytes: u64) {
        self.delete
            .undoable
            .entry(plan)
            .or_insert(Undoable {
                mode,
                count: Count::default(),
            })
            .count
            .add(bytes);
    }

    /// A staged item of `plan` was deleted: it can no longer be put back.
    pub(super) fn forget_undoable(&mut self, plan: u64, bytes: u64) {
        if let Some(u) = self.delete.undoable.get_mut(&plan) {
            u.count.remove(bytes);
            if u.count.items == 0 {
                self.delete.undoable.remove(&plan);
            }
        }
    }

    pub(super) fn refresh_undo(&self, m: &mut Model) {
        m.status.undo = self
            .delete
            .undoable
            .iter()
            .next_back()
            .map(|(&plan, u)| UndoInfo {
                plan,
                mode: u.mode,
                items: u.count.items,
                bytes: u.count.bytes,
            });
    }

    /// Rescans the nearest directory in the tree above each item an undo put back: the
    /// highest of them only, since a rescan covers what is below it.
    pub(super) fn rescan_paths(&mut self, m: &mut Model) {
        let mut dirs: Vec<Ix> = std::mem::take(&mut self.rescan_paths)
            .iter()
            .filter_map(|p| p.parent().and_then(|p| node_at(&m.tree, &p)))
            .collect();
        dirs.sort_unstable();
        dirs.dedup();
        let tops: Vec<Ix> = dirs
            .iter()
            .copied()
            .filter(|&d| {
                !dirs
                    .iter()
                    .any(|&a| a != d && Self::is_ancestor(&m.tree, a, d))
            })
            .collect();
        for d in tops {
            self.delete.deferred_rescans.push(m.tree.node_ref(d));
        }
        self.retry_deferred_rescans(m);
    }
}

/// The deepest visible node on `path`: the node itself when the tree has it, else its nearest
/// ancestor that it has.
fn node_at(tree: &Tree, path: &HostPath) -> Option<Ix> {
    let (root, rest) = tree
        .roots()
        .filter(|&r| tree.is_visible(r))
        .find_map(|r| Some((r, path.rel(&tree.host_path(r))?)))?;
    let mut at = root;
    for name in rest {
        match tree.children(at).find(|&ch| tree.name(ch) == name) {
            Some(ch) if tree.kind(ch) == Kind::Dir => at = ch,
            _ => break,
        }
    }
    Some(at)
}
