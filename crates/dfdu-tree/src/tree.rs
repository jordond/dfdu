use crate::pages::{KidArena, NameArena, Pages};
use crate::{Class, ErrKind, Host, Kind, Totals};

mod classes;
mod dir;
mod edit;
mod extents;
mod paths;

pub use dir::{Children, DirRec, DirState};
use edit::FreeWork;
pub use edit::FreedLink;
use extents::Extents;

const NONE: u32 = u32::MAX;

/// Separator used when joining node names into paths. On Windows it must be `\`: canonical
/// paths there carry the `\\?\` prefix, under which `/` is not a separator.
pub const SEP: u8 = std::path::MAIN_SEPARATOR as u8;

/// Index of a slot. Valid only while the caller holds the lock it was obtained under; use
/// [`NodeRef`] to refer to a node beyond that.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Ix(pub u32);

/// A reference to a node that stays safe to hold across slices. A slot's generation is
/// incremented when it is freed, so a stale reference never resolves to a later occupant.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeRef {
    index: u32,
    generation: u32,
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

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeFlags(u16);

impl NodeFlags {
    pub const HARDLINK: u16 = 1;
    /// Hard link whose bytes are counted (exactly one per inode and scope).
    pub const CHARGED: u16 = 2;
    /// Root of a detached subtree.
    pub const DETACHED: u16 = 4;
    pub const FREE: u16 = 8;
    /// Root of the visible tree.
    pub const VISIBLE_ROOT: u16 = 16;
    /// In a delete plan that has not finished.
    pub const DELETING: u16 = 32;
    /// Directory whose mount differs from its parent's (counted in `Totals::mounts`).
    pub const MOUNT_ROOT: u16 = 64;
    /// btrfs subvolume root; its record is in the model's subvolume table.
    pub const SUBVOL: u16 = 128;
    /// Read-only btrfs subvolume root (counted in `Totals::ro_subvols`).
    pub const RO_SUBVOL: u16 = 256;
    /// Mount of something already scanned elsewhere in the job; not descended into.
    pub const DUP_MOUNT: u16 = 512;
    /// File whose extents were measured: its own disk usage is its unshared bytes instead of its
    /// allocation.
    pub const MEASURED: u16 = 1024;
    /// Measured file with compressed extents whose on-disk size is not known (an upper bound).
    pub const COMPRESSED: u16 = 2048;
    /// Mount of a network filesystem, not descended into (scanned only when asked for).
    pub const NETWORK: u16 = 4096;
    /// Mount of a kernel filesystem (`/proc`, `/sys`), never descended into.
    pub const KERNEL: u16 = 8192;
    /// Top of a protected subtree (`dfdu-rules` protected paths): nothing in it is offered for
    /// deletion, except below an [`NodeFlags::EXCEPTED`] node.
    pub const PROTECTED: u16 = 16384;
    /// Top of a subtree an exception lifts out of the protected subtree around it.
    pub const EXCEPTED: u16 = 32768;

    #[must_use]
    pub fn has(self, f: u16) -> bool {
        self.0 & f != 0
    }
    fn set(&mut self, f: u16, on: bool) {
        if on { self.0 |= f } else { self.0 &= !f }
    }
}

/// One slot, kept to 56 bytes: memory per entry decides how large a tree fits (the target is
/// under 100 MB for 1M entries). Fields most entries do not need live in side tables.
#[derive(Clone, Debug)]
struct Node {
    /// Handle of the name in [`Tree::names`].
    name: u64,
    apparent: u64,
    mtime: i64,
    ino: u64,
    /// Allocation in 512-byte blocks, packed by [`pack_alloc`]; `ALLOC_BIG` when it is in
    /// [`Tree::big_allocs`].
    blocks: u32,
    generation: u32,
    parent: u32,
    dir: u32,
    /// Index into [`Tree::devs`].
    dev: u32,
    flags: NodeFlags,
    /// The node's own error, packed by [`pack_err`]; 0 for none.
    error: u8,
    kind: Kind,
}

const _: () = assert!(std::mem::size_of::<Node>() <= 56);

/// [`Node::blocks`] of an allocation that is not a whole number of 512-byte blocks below
/// 2 TiB. Unix backends report whole blocks, and Windows whole clusters, except NTFS files whose
/// data is inside their MFT record (up to about 700 bytes), whose allocation is a multiple of 8
/// and goes to the side map; files of 2 TiB or more are rare.
const ALLOC_BIG: u32 = u32::MAX;

/// Packs an allocation into a node's block count; `None` when it needs [`Tree::big_allocs`].
fn pack_alloc(alloc: u64) -> Option<u32> {
    if !alloc.is_multiple_of(512) {
        return None;
    }
    u32::try_from(alloc / 512).ok().filter(|&b| b != ALLOC_BIG)
}

/// Error codes in [`Node::error`]. OS error numbers are stored above `ERR_OS`; those that do
/// not fit (above 249, which Linux and macOS never use) go to [`Tree::big_errors`] under
/// `ERR_BIG`.
const ERR_PERMISSION: u8 = 1;
const ERR_NOT_FOUND: u8 = 2;
const ERR_STALLED: u8 = 3;
const ERR_OTHER: u8 = 4;
const ERR_OS: u8 = 5;
const ERR_BIG: u8 = u8::MAX;

/// Packs an error into a node's code; `None` when it needs [`Tree::big_errors`].
fn pack_err(e: ErrKind) -> Option<u8> {
    Some(match e {
        ErrKind::PermissionDenied => ERR_PERMISSION,
        ErrKind::NotFound => ERR_NOT_FOUND,
        ErrKind::Stalled => ERR_STALLED,
        ErrKind::Other => ERR_OTHER,
        ErrKind::Os(code) => {
            let c = u8::try_from(code).ok()?.checked_add(ERR_OS)?;
            if c == ERR_BIG {
                return None;
            }
            c
        }
    })
}

/// The node store. See the crate docs for who may mutate it.
#[derive(Default)]
pub struct Tree {
    nodes: Pages<Node>,
    /// Parallel to `nodes` once any node has shared-extent bytes; empty until then.
    extents: Pages<Extents>,
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

    #[must_use]
    pub fn name_bytes(&self) -> (u64, u64) {
        (self.names.bytes(), self.names_garbage)
    }

    pub fn roots(&self) -> impl Iterator<Item = Ix> + '_ {
        self.roots.iter().map(|&i| Ix(i))
    }

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

    fn alloc_of(&self, i: u32, n: &Node) -> u64 {
        match n.blocks {
            ALLOC_BIG => self.big_allocs[&i],
            b => u64::from(b) * 512,
        }
    }

    fn set_alloc(&mut self, i: u32, alloc: u64) {
        self.nodes[i as usize].blocks = if let Some(b) = pack_alloc(alloc) {
            if !self.big_allocs.is_empty() {
                self.big_allocs.remove(&i);
            }
            b
        } else {
            self.big_allocs.insert(i, alloc);
            ALLOC_BIG
        };
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
    pub fn error(&self, ix: Ix) -> Option<ErrKind> {
        Some(match self.nodes[ix.0 as usize].error {
            0 => return None,
            ERR_PERMISSION => ErrKind::PermissionDenied,
            ERR_NOT_FOUND => ErrKind::NotFound,
            ERR_STALLED => ErrKind::Stalled,
            ERR_OTHER => ErrKind::Other,
            ERR_BIG => ErrKind::Os(self.big_errors[&ix.0]),
            c => ErrKind::Os(i32::from(c - ERR_OS)),
        })
    }

    fn set_error_code(&mut self, i: u32, err: Option<ErrKind>) {
        self.big_errors.remove(&i);
        self.nodes[i as usize].error = match err {
            None => 0,
            Some(e) => pack_err(e).unwrap_or_else(|| {
                let ErrKind::Os(code) = e else {
                    unreachable!("only OS error numbers are too large")
                };
                self.big_errors.insert(i, code);
                ERR_BIG
            }),
        };
    }

    fn dev_index(&mut self, dev: u64) -> u32 {
        if let Some((last, d)) = self.last_dev
            && last == dev
        {
            return d;
        }
        let d = if let Some(&d) = self.dev_index.get(&dev) {
            d
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "every device is some node's, and nodes are numbered by u32"
            )]
            let d = self.devs.len() as u32;
            self.devs.push(dev);
            self.dev_index.insert(dev, d);
            d
        };
        self.last_dev = Some((dev, d));
        d
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
