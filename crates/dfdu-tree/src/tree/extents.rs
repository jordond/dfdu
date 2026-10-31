//! Measured and shared-extent bytes, in a side table holding only the nodes that have some.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use super::{Ix, NodeFlags, Tree};

/// Hashes a slot number. [`Tree::own`] looks a node up for every total it computes, so this
/// is a multiply, not `SipHash`.
#[derive(Default)]
pub(super) struct SlotHasher(u64);

impl Hasher for SlotHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b.into());
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n.into());
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub(super) type ExtentMap = HashMap<u32, Extents, BuildHasherDefault<SlotHasher>>;

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
        if self.extents.is_empty() {
            return Extents::default();
        }
        self.extents.get(&i).copied().unwrap_or_default()
    }

    /// The node's shared-extent bytes, creating its entry on first use.
    pub(super) fn ext_mut(&mut self, i: u32) -> &mut Extents {
        self.extents.entry(i).or_default()
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
            if x.excl == 0 && x.shared == 0 && x.shared_out == 0 {
                t.extents.remove(&ix.0);
            }
        });
    }
}

#[cfg(test)]
mod tests;
