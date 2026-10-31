use crate::pages::{KidArena, NameArena, Pages};
use crate::{Class, Host, Kind, Totals};

mod classes;
mod dir;
mod edit;
mod extents;
mod node;
mod paths;
mod refs;

pub use dir::{Children, DirRec, DirState};
use edit::FreeWork;
pub use edit::FreedLink;
use node::Node;
pub use node::NodeFlags;
pub use refs::NodeRef;

const NONE: u32 = u32::MAX;

/// Separator used when joining node names into paths. On Windows it must be `\`: canonical
/// paths there carry the `\\?\` prefix, under which `/` is not a separator.
pub const SEP: u8 = std::path::MAIN_SEPARATOR as u8;

/// Index of a slot. Valid only while the caller holds the lock it was obtained under; use
/// [`NodeRef`] to refer to a node beyond that.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Ix(pub u32);

/// The node store. See the crate docs for who may mutate it.
#[derive(Default)]
pub struct Tree {
    nodes: Pages<Node>,
    /// Shared-extent bytes, by slot, of the nodes that have any: measured files and the nodes
    /// shared bytes are counted at, a small part of the tree. A table parallel to `nodes` cost
    /// 24 bytes for every node once one file was measured.
    extents: extents::ExtentMap,
    /// Device numbers the nodes' `dev` indexes, and the reverse map.
    devs: Vec<u64>,
    dev_index: std::collections::HashMap<u64, u32>,
    /// The last device looked up: nearly every entry is on the same device as the one before.
    last_dev: Option<(u64, u32)>,
    /// OS error numbers too large for a node's error code, by slot.
    big_errors: std::collections::HashMap<u32, i32>,
    /// Allocations a node's block count cannot hold, by slot.
    big_allocs: std::collections::HashMap<u32, u64>,
    /// The rule each classified node matched, by slot; few nodes have one.
    classes: std::collections::HashMap<u32, Class>,
    dirs: Pages<DirRec>,
    kids: KidArena,
    free_nodes: Vec<u32>,
    free_dirs: Vec<u32>,
    names: NameArena,
    names_garbage: u64,
    roots: Vec<u32>,
    /// The host the paths are on: this machine, unless the tree came from an agent or a file.
    host: Host,
    version: u64,
    free_work: Vec<FreeWork>,
    live: u64,
}

impl Tree {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The host the tree's paths name files on.
    #[must_use]
    pub fn host(&self) -> Host {
        self.host
    }

    pub fn set_host(&mut self, host: Host) {
        self.host = host;
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn live_nodes(&self) -> u64 {
        self.live
    }

    /// Bytes in the name arena, freed names included, and how many of them freed names left
    /// unused: `(stored, garbage)`.
    #[must_use]
    pub fn name_storage(&self) -> (u64, u64) {
        (self.names.bytes(), self.names_garbage)
    }

    pub fn roots(&self) -> impl Iterator<Item = Ix> + '_ {
        self.roots.iter().map(|&i| Ix(i))
    }

    #[must_use]
    pub fn parent(&self, ix: Ix) -> Option<Ix> {
        let p = self.nodes[ix.0 as usize].parent;
        (p != NONE).then_some(Ix(p))
    }

    #[must_use]
    pub fn name(&self, ix: Ix) -> &[u8] {
        self.names.get(self.nodes[ix.0 as usize].name)
    }

    #[must_use]
    pub fn kind(&self, ix: Ix) -> Kind {
        self.nodes[ix.0 as usize].kind
    }

    #[must_use]
    pub fn flags(&self, ix: Ix) -> NodeFlags {
        self.nodes[ix.0 as usize].flags
    }

    /// Size fields as scanned: `(apparent, alloc)`, before hard-link charging.
    #[must_use]
    pub fn raw_size(&self, ix: Ix) -> (u64, u64) {
        let n = &self.nodes[ix.0 as usize];
        (n.apparent, self.alloc_of(ix.0, n))
    }

    #[must_use]
    pub fn mtime(&self, ix: Ix) -> i64 {
        self.nodes[ix.0 as usize].mtime
    }

    #[must_use]
    pub fn dev_ino(&self, ix: Ix) -> (u64, u64) {
        let n = &self.nodes[ix.0 as usize];
        (self.devs[n.dev as usize], n.ino)
    }

    #[must_use]
    pub fn dir(&self, ix: Ix) -> Option<&DirRec> {
        let d = self.nodes[ix.0 as usize].dir;
        (d != NONE).then(|| &self.dirs[d as usize])
    }

    pub fn dir_mut(&mut self, ix: Ix) -> Option<&mut DirRec> {
        let d = self.nodes[ix.0 as usize].dir;
        (d != NONE).then(|| &mut self.dirs[d as usize])
    }

    pub fn children(&self, ix: Ix) -> Children<'_> {
        let (head, more) = match self.dir(ix) {
            Some(d) => self.kids.parts(&d.kids),
            None => (&[][..], &[][..]),
        };
        Children {
            left: head.len() + more.iter().map(Vec::len).sum::<usize>(),
            head: head.iter(),
            more: more.iter(),
        }
    }

    /// Calls `f` with every node at or below `ix` that is not a directory: `ix` itself when it
    /// has no directory record, else every such node in its subtree, in no particular order.
    pub fn for_each_leaf(&self, ix: Ix, mut f: impl FnMut(Ix)) {
        if self.dir(ix).is_none() {
            f(ix);
            return;
        }
        let mut stack = vec![ix];
        while let Some(d) = stack.pop() {
            for c in self.children(d) {
                if self.dir(c).is_some() {
                    stack.push(c);
                } else {
                    f(c);
                }
            }
        }
    }

    /// Room for `n` more children of `ix`, so the children of a chunk are stored together. A
    /// directory's first chunk reserves exactly what it needs: most directories arrive in one
    /// chunk. Later chunks add blocks at least as large as the list so far (`KidArena`).
    ///
    /// # Panics
    ///
    /// `ix` is not a directory.
    pub fn reserve_children(&mut self, ix: Ix, n: usize) {
        let d = self.nodes[ix.0 as usize].dir;
        assert!(d != NONE, "children reserved for a non-directory");
        self.kids.reserve(&mut self.dirs[d as usize].kids, n);
    }

    /// Slots of the child lists in shared pages (the blocks of directories that took several
    /// chunks are not counted), and how many of them lists that were freed left unused.
    #[must_use]
    pub fn child_slots(&self) -> (u64, u64) {
        self.kids.slots()
    }

    /// What this node contributes by itself (one item, its own error, its own bytes unless it
    /// is an uncharged hard link).
    #[must_use]
    pub fn own(&self, ix: Ix) -> Totals {
        let n = &self.nodes[ix.0 as usize];
        let x = self.ext(ix.0);
        let counted = !n.flags.has(NodeFlags::HARDLINK) || n.flags.has(NodeFlags::CHARGED);
        let alloc = self.alloc_of(ix.0, n);
        let base = if n.flags.has(NodeFlags::MEASURED) {
            x.excl
        } else {
            alloc
        };
        Totals {
            apparent: if counted { n.apparent } else { 0 },
            alloc: if counted { base } else { 0 } + x.shared,
            referenced: if counted { alloc } else { 0 },
            shared_out: x.shared_out,
            items: 1,
            errors: u64::from(n.error != 0),
            mounts: u64::from(n.flags.has(NodeFlags::MOUNT_ROOT)),
            ro_subvols: u64::from(n.flags.has(NodeFlags::RO_SUBVOL)),
        }
    }

    /// Totals of the node and everything below it.
    #[must_use]
    pub fn total(&self, ix: Ix) -> Totals {
        match self.dir(ix) {
            Some(d) => d.total,
            None => self.own(ix),
        }
    }

    // For the invariant checker.
    pub(crate) fn slot_count(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn raw_parent(&self, i: u32) -> u32 {
        self.nodes[i as usize].parent
    }

    pub(crate) fn is_free_slot(&self, i: u32) -> bool {
        self.nodes[i as usize].flags.has(NodeFlags::FREE)
    }
}

#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
