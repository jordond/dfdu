//! Measured and shared-extent bytes, in a side table that stays empty until a node has some.

use super::{Ix, NodeFlags, Tree};

/// Shared-extent bytes of a node; zero except on filesystems that share extents.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct Extents {
    /// Unshared bytes of a `MEASURED` file.
    pub(super) excl: u64,
    /// Shared bytes counted at this node: data whose references in the scan are all below it
    /// and not all below one child.
    pub(super) shared: u64,
    /// The part of `shared` that no other file in the scan references: something outside the
    /// scan does, so deleting this node does not free it.
    pub(super) shared_out: u64,
}

impl Tree {
    pub(super) fn ext(&self, i: u32) -> Extents {
        self.extents.get(i as usize).copied().unwrap_or_default()
    }

    /// The node's shared-extent bytes, creating the side table on first use.
    pub(super) fn ext_mut(&mut self, i: u32) -> &mut Extents {
        self.extents.grow_to(self.nodes.len(), Extents::default());
        &mut self.extents[i as usize]
    }

    /// Records a file's measured unshared bytes ([`NodeFlags::MEASURED`]).
    pub fn set_measured(&mut self, ix: Ix, excl: u64, compressed: bool) {
        self.restate(ix, |t| {
            t.ext_mut(ix.0).excl = excl;
            let n = &mut t.nodes[ix.0 as usize];
            n.flags.set(NodeFlags::MEASURED, true);
            n.flags.set(NodeFlags::COMPRESSED, compressed);
        });
    }

    /// Unshared bytes of a measured file.
    #[must_use]
    pub fn excl(&self, ix: Ix) -> u64 {
        self.ext(ix.0).excl
    }

    /// Shared bytes counted at this node.
    #[must_use]
    pub fn shared_here(&self, ix: Ix) -> u64 {
        self.ext(ix.0).shared
    }

    /// Adds shared bytes counted at this node; `out` when nothing else in the scan
    /// references them.
    pub fn add_shared(&mut self, ix: Ix, bytes: u64, out: bool) {
        self.restate(ix, |t| {
            let x = t.ext_mut(ix.0);
            x.shared += bytes;
            if out {
                x.shared_out += bytes;
            }
        });
    }

    pub fn sub_shared(&mut self, ix: Ix, bytes: u64, out: bool) {
        self.restate(ix, |t| {
            let x = t.ext_mut(ix.0);
            debug_assert!(
                x.shared >= bytes && (!out || x.shared_out >= bytes),
                "shared bytes underflow"
            );
            x.shared = x.shared.saturating_sub(bytes);
            if out {
                x.shared_out = x.shared_out.saturating_sub(bytes);
            }
        });
    }
}

#[cfg(test)]
mod tests;
