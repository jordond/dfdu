//! References to nodes that outlive a lock, and resolving them back to slots.

use super::{Ix, NONE, NodeFlags, Tree};

/// A reference to a node that stays safe to hold across slices. A slot's generation is
/// incremented when it is freed, so a stale reference never resolves to a later occupant.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeRef {
    pub(super) index: u32,
    pub(super) generation: u32,
}

impl NodeRef {
    /// The reference as one number, for a peer that only hands it back ([`NodeRef::from_bits`]).
    #[must_use]
    pub fn to_bits(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.index)
    }

    /// A reference from [`NodeRef::to_bits`]. Any number gives a reference; one that names no
    /// live node resolves to nothing, as a stale one does.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the index is the low 32 bits, on purpose"
    )]
    pub fn from_bits(bits: u64) -> NodeRef {
        NodeRef {
            index: bits as u32,
            generation: (bits >> 32) as u32,
        }
    }
}

impl Tree {
    #[must_use]
    pub fn node_ref(&self, ix: Ix) -> NodeRef {
        NodeRef {
            index: ix.0,
            generation: self.nodes[ix.0 as usize].generation,
        }
    }

    /// Resolves a reference to a live, attached node (visible or in a shadow subtree).
    #[must_use]
    pub fn resolve(&self, r: NodeRef) -> Option<Ix> {
        let n = self.nodes.get(r.index as usize)?;
        if n.generation != r.generation || n.flags.has(NodeFlags::FREE) {
            return None;
        }
        self.top(Ix(r.index)).map(|_| Ix(r.index))
    }

    /// [`Tree::resolve`], and `None` also when the node or an ancestor is a directory marked
    /// `cut` (the cut check of the applier's gate), in one walk up the tree. At 10M entries the
    /// nodes of a walk are rarely in the cache, so each walk costs a cache miss per level.
    #[must_use]
    pub fn resolve_uncut(&self, r: NodeRef) -> Option<Ix> {
        let n = self.nodes.get(r.index as usize)?;
        if n.generation != r.generation {
            return None;
        }
        let mut i = r.index;
        loop {
            let n = &self.nodes[i as usize];
            if n.flags.has(NodeFlags::DETACHED) || n.flags.has(NodeFlags::FREE) {
                return None;
            }
            if n.dir != NONE && self.dirs[n.dir as usize].cut {
                return None;
            }
            if n.parent == NONE {
                return Some(Ix(r.index));
            }
            i = n.parent;
        }
    }

    /// [`Tree::resolve`] and [`Tree::ancestors`] in one walk: the node and its ancestors into
    /// `out`, topmost first, if it resolves.
    pub fn resolve_ancestors(&self, r: NodeRef, out: &mut Vec<Ix>) -> Option<Ix> {
        out.clear();
        let n = self.nodes.get(r.index as usize)?;
        if n.generation != r.generation {
            return None;
        }
        let mut i = r.index;
        loop {
            let n = &self.nodes[i as usize];
            if n.flags.has(NodeFlags::DETACHED) || n.flags.has(NodeFlags::FREE) {
                out.clear();
                return None;
            }
            out.push(Ix(i));
            if n.parent == NONE {
                out.reverse();
                return Some(Ix(r.index));
            }
            i = n.parent;
        }
    }

    /// Whether `r` names a node that has been freed (its slot may hold another node by now).
    /// A node in a detached subtree that is not freed yet does not resolve, but is not freed.
    #[must_use]
    pub fn is_freed(&self, r: NodeRef) -> bool {
        self.nodes
            .get(r.index as usize)
            .is_none_or(|n| n.generation != r.generation || n.flags.has(NodeFlags::FREE))
    }

    /// Resolves a reference to a node reachable from a visible root.
    #[must_use]
    pub fn resolve_visible(&self, r: NodeRef) -> Option<Ix> {
        let ix = self.resolve(r)?;
        let top = self.top(ix)?;
        self.nodes[top.0 as usize]
            .flags
            .has(NodeFlags::VISIBLE_ROOT)
            .then_some(ix)
    }

    /// Topmost ancestor of `ix`, or `None` if `ix` is inside a detached subtree.
    fn top(&self, ix: Ix) -> Option<Ix> {
        let mut i = ix.0;
        loop {
            let n = &self.nodes[i as usize];
            if n.flags.has(NodeFlags::DETACHED) || n.flags.has(NodeFlags::FREE) {
                return None;
            }
            if n.parent == NONE {
                return Some(Ix(i));
            }
            i = n.parent;
        }
    }

    #[must_use]
    pub fn is_visible(&self, ix: Ix) -> bool {
        self.top(ix)
            .is_some_and(|t| self.nodes[t.0 as usize].flags.has(NodeFlags::VISIBLE_ROOT))
    }
}

#[cfg(test)]
mod tests;
