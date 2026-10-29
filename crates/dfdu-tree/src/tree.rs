use std::path::PathBuf;

use fixedbitset::FixedBitSet;

use crate::pages::{KidArena, Kids, NameArena, Pages};
use crate::{Class, DirId, ErrKind, JobId, Kind, Meta, Totals, name_os};

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

/// Scan state of a directory. Everything but `Pending` is terminal.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DirState {
    Pending,
    Complete,
    Partial,
    Failed,
    Skipped,
    Cancelled,
    Abandoned,
    Lost,
    Superseded,
    Deleting,
    Deleted,
}

impl DirState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        self != DirState::Pending
    }
}

/// Per-directory record. The scan bookkeeping fields are owned by the applier.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent scan bookkeeping flags, each set at its own point by the applier"
)]
pub struct DirRec {
    pub(crate) kids: Kids,
    pub(crate) total: Totals,
    pub state: DirState,
    /// State taken when `pending` reaches zero, set when the listing closes.
    pub outcome: DirState,
    pub job: JobId,
    pub dir_id: DirId,
    pub pending: u32,
    pub listing_closed: bool,
    pub reported: bool,
    pub cut: bool,
    pub stalled: bool,
    pub chunks_seen: u32,
    /// Chunk sequence numbers applied, for dropping duplicates: 0-63 in `seen_low`, the rest
    /// in `seen_high`, which stays unallocated for all but wide directories (a directory
    /// takes one chunk per 4,096 entries). Freed when the listing closes ([`DirRec::close`]),
    /// after which no chunk is applied.
    seen_low: u64,
    seen_high: FixedBitSet,
    pub changed_at: u64,
}

impl DirRec {
    fn new() -> Self {
        DirRec {
            kids: Kids::default(),
            total: Totals::default(),
            state: DirState::Pending,
            outcome: DirState::Complete,
            job: JobId(0),
            dir_id: DirId::ROOT,
            pending: 1,
            listing_closed: false,
            reported: false,
            cut: false,
            stalled: false,
            chunks_seen: 0,
            seen_low: 0,
            seen_high: FixedBitSet::new(),
            changed_at: 0,
        }
    }

    /// Records chunk `seq` as seen. Returns false if it was already seen.
    pub fn mark_seq(&mut self, seq: u32) -> bool {
        if seq < 64 {
            let bit = 1 << seq;
            let new = self.seen_low & bit == 0;
            self.seen_low |= bit;
            return new;
        }
        let i = (seq - 64) as usize;
        self.seen_high.grow(i + 1);
        !self.seen_high.put(i)
    }

    /// Closes the listing. No more chunks are applied, so the duplicate filter is freed and
    /// the slack a directory of several chunks left in its child list is returned.
    pub fn close(&mut self, outcome: DirState) {
        self.listing_closed = true;
        self.outcome = outcome;
        self.seen_high = FixedBitSet::new();
        self.kids.shrink();
    }
}

/// Iterator over a directory's children ([`Tree::children`]).
pub struct Children<'a> {
    head: std::slice::Iter<'a, u32>,
    more: std::slice::Iter<'a, Vec<u32>>,
    left: usize,
}

impl Iterator for Children<'_> {
    type Item = Ix;

    fn next(&mut self) -> Option<Ix> {
        loop {
            if let Some(&c) = self.head.next() {
                self.left -= 1;
                return Some(Ix(c));
            }
            self.head = self.more.next()?.iter();
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

impl ExactSizeIterator for Children<'_> {}

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

/// Shared-extent bytes of a node; zero except on filesystems that share extents.
#[derive(Copy, Clone, Debug, Default)]
struct Extents {
    /// Unshared bytes of a `MEASURED` file.
    excl: u64,
    /// Shared bytes counted at this node: data whose references in the scan are all below it
    /// and not all below one child.
    shared: u64,
    /// The part of `shared` that no other file in the scan references: something outside the
    /// scan does, so deleting this node does not free it.
    shared_out: u64,
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

/// A hard-link or measured file reported while freeing a detached subtree, so the caller can
/// update its link and extent tables.
#[derive(Copy, Clone, Debug)]
pub struct FreedLink {
    pub dev: u64,
    pub ino: u64,
    pub node: NodeRef,
    pub charged: bool,
    pub hardlink: bool,
    pub measured: bool,
}

#[derive(Copy, Clone, Debug)]
enum FreeWork {
    Node(u32),
    Expanded(u32),
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
    version: u64,
    free_work: Vec<FreeWork>,
    live: u64,
}

impl Tree {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    fn ext(&self, i: u32) -> Extents {
        self.extents.get(i as usize).copied().unwrap_or_default()
    }

    /// The node's shared-extent bytes, creating the side table on first use.
    fn ext_mut(&mut self, i: u32) -> &mut Extents {
        self.extents.grow_to(self.nodes.len(), Extents::default());
        &mut self.extents[i as usize]
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

    /// Filesystem path of the node: the root's path joined with each name below it. Use this for
    /// anything handed to the OS; [`Tree::path`] is a byte key for ordering and lookups.
    #[must_use]
    pub fn path_buf(&self, ix: Ix) -> PathBuf {
        let mut parts = Vec::new();
        self.ancestors(ix, &mut parts);
        let mut out = PathBuf::new();
        for p in parts {
            out.push(&*name_os(self.name(p)));
        }
        out
    }

    /// Path from the topmost ancestor as bytes, components joined with [`SEP`]: a key for
    /// ordering (hard-link charging) and lookups, not for passing to the OS.
    #[must_use]
    pub fn path(&self, ix: Ix) -> Vec<u8> {
        let mut parts = Vec::new();
        self.ancestors(ix, &mut parts);
        let mut out = Vec::new();
        for (i, p) in parts.into_iter().enumerate() {
            let name = self.name(p);
            if i > 0 && out.last() != Some(&SEP) {
                out.push(SEP);
            }
            out.extend_from_slice(name);
        }
        out
    }

    /// The node and its ancestors into `out`, topmost first: the components of [`Tree::path`].
    pub fn ancestors(&self, ix: Ix, out: &mut Vec<Ix>) {
        out.clear();
        out.push(ix);
        let mut cur = ix;
        while let Some(p) = self.parent(cur) {
            out.push(p);
            cur = p;
        }
        out.reverse();
    }

    /// Compares two nodes' paths as `self.path(a).cmp(&self.path(b))` would, from their
    /// [`Tree::ancestors`], without building the paths. Ancestors the two share are skipped:
    /// they contribute the same bytes to both, so the order is decided by what follows them.
    #[must_use]
    pub fn cmp_paths(&self, a: &[Ix], b: &[Ix]) -> std::cmp::Ordering {
        let k = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        // The last byte written before component `k`, which decides whether a separator
        // precedes it; the same for both.
        let last = match k {
            0 => None,
            _ => self.name(a[k - 1]).last().copied().or(Some(SEP)),
        };
        self.path_bytes(&a[k..], k == 0, last)
            .cmp(self.path_bytes(&b[k..], k == 0, last))
    }

    /// The bytes [`Tree::path`] writes for `parts`, given the last byte written before them and
    /// whether `parts[0]` is the topmost component.
    fn path_bytes<'a>(
        &'a self,
        parts: &'a [Ix],
        top: bool,
        mut last: Option<u8>,
    ) -> impl Iterator<Item = u8> + 'a {
        parts.iter().enumerate().flat_map(move |(i, &p)| {
            let name = self.name(p);
            let sep = (i > 0 || !top) && last != Some(SEP);
            last = match name.last() {
                Some(&c) => Some(c),
                None if sep => Some(SEP),
                None => last,
            };
            sep.then_some(SEP).into_iter().chain(name.iter().copied())
        })
    }

    fn alloc_node(&mut self, parent: u32, name: &[u8], meta: &Meta, flags: u16) -> u32 {
        // Names are never truncated: a cut name would not convert back (`name_os`). Entry names
        // are at most a few hundred bytes; root paths can be longer, up to 16 MiB.
        let name = self.names.push(name);
        let dev = self.dev_index(meta.dev);
        let mut f = NodeFlags::default();
        f.set(flags, true);
        if meta.is_hardlink() {
            f.set(NodeFlags::HARDLINK, true);
        }
        let generation = match self.free_nodes.last() {
            Some(&i) => self.nodes[i as usize].generation,
            None => 0,
        };
        let node = Node {
            name,
            apparent: meta.apparent,
            mtime: meta.mtime,
            ino: meta.ino,
            blocks: 0,
            generation,
            parent,
            dir: NONE,
            dev,
            flags: f,
            error: 0,
            kind: meta.kind,
        };
        self.live += 1;
        let i = if let Some(i) = self.free_nodes.pop() {
            self.nodes[i as usize] = node;
            self.big_errors.remove(&i);
            self.classes.remove(&i);
            i
        } else {
            assert!(
                self.nodes.len() < NONE as usize,
                "fewer than 2^32 - 1 nodes"
            );
            self.nodes.push(node);
            #[expect(clippy::cast_possible_truncation, reason = "asserted just above")]
            let i = (self.nodes.len() - 1) as u32;
            i
        };
        self.set_alloc(i, meta.alloc);
        if !self.extents.is_empty() {
            *self.ext_mut(i) = Extents::default();
        }
        i
    }

    fn alloc_dir(&mut self, node: u32) {
        let rec = DirRec::new();
        let d = if let Some(d) = self.free_dirs.pop() {
            self.dirs[d as usize] = rec;
            d
        } else {
            assert!(
                self.dirs.len() < NONE as usize,
                "fewer than 2^32 - 1 directories"
            );
            self.dirs.push(rec);
            #[expect(clippy::cast_possible_truncation, reason = "asserted just above")]
            let d = (self.dirs.len() - 1) as u32;
            d
        };
        self.nodes[node as usize].dir = d;
        self.dirs[d as usize].total = self.own(Ix(node));
    }

    /// Creates a root directory. Visible roots are what the UI shows; non-visible roots hold
    /// rescan shadows until they are swapped in.
    pub fn new_root(&mut self, name: &[u8], meta: &Meta, visible: bool) -> Ix {
        let flags = if visible { NodeFlags::VISIBLE_ROOT } else { 0 };
        let i = self.alloc_node(NONE, name, meta, flags);
        self.alloc_dir(i);
        if visible {
            self.roots.push(i);
        }
        Ix(i)
    }

    /// Adds a directory placeholder under `parent`. The caller propagates the new child's
    /// totals with [`Tree::propagate_add`] before releasing the write lock.
    pub fn add_dir(&mut self, parent: Ix, name: &[u8], meta: &Meta) -> Ix {
        self.add_dir_flagged(parent, name, meta, 0)
    }

    /// [`Tree::add_dir`] with flags set from the start (`MOUNT_ROOT`, `DUP_MOUNT`), so they
    /// are part of the directory's own totals.
    pub fn add_dir_flagged(&mut self, parent: Ix, name: &[u8], meta: &Meta, flags: u16) -> Ix {
        let i = self.alloc_node(parent.0, name, meta, flags);
        self.alloc_dir(i);
        self.push_child(parent, i);
        Ix(i)
    }

    /// Adds a non-directory under `parent`. `charged` applies to hard links only. The caller
    /// propagates, as for [`Tree::add_dir`].
    pub fn add_file(
        &mut self,
        parent: Ix,
        name: &[u8],
        meta: &Meta,
        charged: bool,
        error: Option<ErrKind>,
    ) -> Ix {
        let extra = if charged && meta.is_hardlink() {
            NodeFlags::CHARGED
        } else {
            0
        };
        let i = self.alloc_node(parent.0, name, meta, extra);
        if error.is_some() {
            self.set_error_code(i, error);
        }
        self.push_child(parent, i);
        Ix(i)
    }

    fn push_child(&mut self, parent: Ix, child: u32) {
        let d = self.nodes[parent.0 as usize].dir;
        assert!(d != NONE, "child added to a non-directory");
        self.kids.push(&mut self.dirs[d as usize].kids, child);
    }

    fn bump(&mut self) -> u64 {
        self.version += 1;
        self.version
    }

    /// Adds `delta` to `dir` and every ancestor, and marks them changed.
    pub fn propagate_add(&mut self, dir: Ix, delta: &Totals) {
        self.propagate(dir, |t| t.add(delta));
    }

    /// Subtracts `delta` from `dir` and every ancestor, and marks them changed.
    pub fn propagate_sub(&mut self, dir: Ix, delta: &Totals) {
        self.propagate(dir, |t| t.sub(delta));
    }

    fn propagate(&mut self, dir: Ix, f: impl Fn(&mut Totals)) {
        let v = self.bump();
        let mut i = dir.0;
        while i != NONE {
            let node = &self.nodes[i as usize];
            let (rec_ix, parent) = (node.dir, node.parent);
            let rec = &mut self.dirs[rec_ix as usize];
            f(&mut rec.total);
            rec.changed_at = v;
            i = parent;
        }
    }

    /// Records an error on the node itself (a directory that could not be read, or an entry
    /// that could not be stat'ed) and counts it in the ancestors.
    pub fn set_error(&mut self, ix: Ix, err: ErrKind) {
        let had = self.nodes[ix.0 as usize].error != 0;
        self.set_error_code(ix.0, Some(err));
        if !had {
            let one = Totals {
                errors: 1,
                ..Totals::default()
            };
            if self.dir(ix).is_some() {
                self.propagate_add(ix, &one);
            } else if let Some(p) = self.parent(ix) {
                self.propagate_add(p, &one);
            }
        }
    }

    /// Changes whether a hard link's bytes are counted, and adjusts the ancestors.
    pub fn set_charged(&mut self, ix: Ix, charged: bool) {
        let n = &self.nodes[ix.0 as usize];
        if !n.flags.has(NodeFlags::HARDLINK) || n.flags.has(NodeFlags::CHARGED) == charged {
            return;
        }
        self.restate(ix, |t| {
            t.nodes[ix.0 as usize]
                .flags
                .set(NodeFlags::CHARGED, charged);
        });
    }

    /// Replaces the node's own stat (a job root's stat arrives in its first chunk) and adjusts
    /// the totals of the node and its ancestors.
    pub fn update_meta(&mut self, ix: Ix, meta: &Meta) {
        let dev = self.dev_index(meta.dev);
        self.restate(ix, |t| {
            t.set_alloc(ix.0, meta.alloc);
            let n = &mut t.nodes[ix.0 as usize];
            n.apparent = meta.apparent;
            n.mtime = meta.mtime;
            n.dev = dev;
            n.ino = meta.ino;
        });
    }

    /// Sets or clears a flag. For flags counted in [`Totals`] (`MOUNT_ROOT`, `RO_SUBVOL`) the
    /// node's and its ancestors' totals follow.
    pub fn set_flag(&mut self, ix: Ix, flag: u16, on: bool) {
        self.restate(ix, |t| t.nodes[ix.0 as usize].flags.set(flag, on));
    }

    /// Sets or clears the rule node `ix` matched.
    pub fn set_class(&mut self, ix: Ix, class: Option<Class>) {
        match class {
            Some(c) => self.classes.insert(ix.0, c),
            None => self.classes.remove(&ix.0),
        };
    }

    #[must_use]
    pub fn class(&self, ix: Ix) -> Option<Class> {
        if self.classes.is_empty() {
            return None;
        }
        self.classes.get(&ix.0).copied()
    }

    /// Every classified node, in no order.
    pub fn classes(&self) -> impl Iterator<Item = (Ix, Class)> + '_ {
        self.classes.iter().map(|(&i, &c)| (Ix(i), c))
    }

    /// Marks node `ix` as the top of a protected subtree, of an exception in one, or neither.
    /// These flags are not counted in totals, so no ancestor changes.
    pub fn set_protection(&mut self, ix: Ix, protected: bool, excepted: bool) {
        let f = &mut self.nodes[ix.0 as usize].flags;
        f.set(NodeFlags::PROTECTED, protected);
        f.set(NodeFlags::EXCEPTED, excepted);
    }

    /// Whether node `ix` is in a protected subtree: the nearest node at or above it marked
    /// [`NodeFlags::PROTECTED`] or [`NodeFlags::EXCEPTED`] is a protected one.
    #[must_use]
    pub fn is_protected(&self, ix: Ix) -> bool {
        let mut cur = Some(ix);
        while let Some(i) = cur {
            let f = self.flags(i);
            if f.has(NodeFlags::PROTECTED) {
                return true;
            }
            if f.has(NodeFlags::EXCEPTED) {
                return false;
            }
            cur = self.parent(i);
        }
        false
    }

    /// The class that applies to node `ix`, and the node it is on: [`Tree::class_below`] taken
    /// down from the root. The nearest classified node at or above `ix` wins, so a match inside
    /// another (pip's cache in the user caches) is what it is; but a match whose rule stops the
    /// descent (`stops`) covers everything below it, so the highest such node wins over any
    /// below it. Those exist when the rule above was decided only after its listing's
    /// subdirectories were queued (`dfdu_scan::classify`).
    #[must_use]
    pub fn class_in_effect(&self, ix: Ix, stops: impl Fn(&Class) -> bool) -> Option<(Ix, Class)> {
        if self.classes.is_empty() {
            return None;
        }
        let mut chain = Vec::new();
        let mut cur = Some(ix);
        while let Some(i) = cur {
            chain.push(i);
            cur = self.parent(i);
        }
        chain
            .into_iter()
            .rev()
            .fold(None, |above, i| self.class_below(above, i, &stops))
    }

    /// The class in effect at node `ix` given `above`, the class in effect at its parent: that
    /// one when its rule stops the descent (`stops`), otherwise `ix`'s own class if it has one,
    /// otherwise still `above`. The one step [`Tree::class_in_effect`] is made of, for walks
    /// that go down the tree anyway.
    #[must_use]
    pub fn class_below(
        &self,
        above: Option<(Ix, Class)>,
        ix: Ix,
        stops: impl Fn(&Class) -> bool,
    ) -> Option<(Ix, Class)> {
        match above {
            Some((_, c)) if stops(&c) => above,
            _ => self.class(ix).map(|c| (ix, c)).or(above),
        }
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

    /// Changes a node and moves the difference in its own totals into its ancestors' totals.
    fn restate(&mut self, ix: Ix, f: impl FnOnce(&mut Self)) {
        let old = self.own(ix);
        f(self);
        let new = self.own(ix);
        if old == new {
            self.bump();
        } else {
            let target = if self.dir(ix).is_some() {
                Some(ix)
            } else {
                self.parent(ix)
            };
            if let Some(t) = target {
                self.propagate(t, |tot| {
                    tot.add(&new);
                    tot.sub(&old);
                });
            }
        }
    }

    pub fn set_deleting(&mut self, ix: Ix, on: bool) {
        self.nodes[ix.0 as usize].flags.set(NodeFlags::DELETING, on);
        self.bump();
    }

    /// Marks the tree changed, for readers comparing versions, without changing a node.
    pub fn bump_version(&mut self) {
        self.bump();
    }

    /// Marks a directory changed without changing totals (state changes the UI should see).
    pub fn touch(&mut self, ix: Ix) {
        let v = self.bump();
        if let Some(d) = self.dir_mut(ix) {
            d.changed_at = v;
        }
    }

    /// Unlinks `ix` from its parent (or from the visible roots), subtracts its totals from the
    /// ancestors and queues the subtree for freeing. References into it stop resolving at once.
    pub fn detach(&mut self, ix: Ix) {
        let total = self.total(ix);
        let parent = self.nodes[ix.0 as usize].parent;
        if parent == NONE {
            self.roots.retain(|&r| r != ix.0);
            self.bump();
        } else {
            self.remove_child(Ix(parent), ix.0);
            self.propagate_sub(Ix(parent), &total);
        }
        let n = &mut self.nodes[ix.0 as usize];
        n.parent = NONE;
        n.flags.set(NodeFlags::DETACHED, true);
        n.flags.set(NodeFlags::VISIBLE_ROOT, false);
        self.free_work.push(FreeWork::Node(ix.0));
    }

    /// Puts the non-visible root `new` in the place of `old` (rescan swap) and detaches `old`.
    ///
    /// # Panics
    ///
    /// `new` is not a non-visible root, or `old` is neither a visible root nor a child of its
    /// parent.
    pub fn replace(&mut self, old: Ix, new: Ix) {
        assert_eq!(
            self.nodes[new.0 as usize].parent, NONE,
            "replacement must be a root"
        );
        assert!(
            !self.nodes[new.0 as usize]
                .flags
                .has(NodeFlags::VISIBLE_ROOT)
        );
        let parent = self.nodes[old.0 as usize].parent;
        let (old_total, new_total) = (self.total(old), self.total(new));
        if parent == NONE {
            #[expect(
                clippy::expect_used,
                reason = "a caller's broken precondition, documented"
            )]
            let pos = self
                .roots
                .iter()
                .position(|&r| r == old.0)
                .expect("visible root");
            self.roots[pos] = new.0;
            self.nodes[new.0 as usize]
                .flags
                .set(NodeFlags::VISIBLE_ROOT, true);
            self.bump();
        } else {
            let d = self.nodes[parent as usize].dir as usize;
            let found = self.kids.replace(&mut self.dirs[d].kids, old.0, new.0);
            assert!(found, "child");
            self.nodes[new.0 as usize].parent = parent;
            self.propagate_sub(Ix(parent), &old_total);
            self.propagate_add(Ix(parent), &new_total);
        }
        let n = &mut self.nodes[old.0 as usize];
        n.parent = NONE;
        n.flags.set(NodeFlags::DETACHED, true);
        n.flags.set(NodeFlags::VISIBLE_ROOT, false);
        self.free_work.push(FreeWork::Node(old.0));
    }

    fn remove_child(&mut self, parent: Ix, child: u32) {
        let d = self.nodes[parent.0 as usize].dir as usize;
        self.kids.remove(&mut self.dirs[d].kids, child);
    }

    /// Frees up to `budget` nodes of detached subtrees, children before parents, so no freed
    /// slot is ever the parent of a node that still exists. Hard links are reported to `on_link`
    /// before their slot is freed. Returns true when no free work remains.
    pub fn free_step(&mut self, budget: usize, mut on_link: impl FnMut(FreedLink)) -> bool {
        let mut units = 0;
        while units < budget {
            let Some(w) = self.free_work.pop() else {
                return true;
            };
            units += 1;
            match w {
                FreeWork::Node(i) => {
                    if self.nodes[i as usize].dir == NONE {
                        self.free_slot(i, &mut on_link);
                    } else {
                        self.free_work.push(FreeWork::Expanded(i));
                    }
                }
                FreeWork::Expanded(i) => {
                    let d = self.nodes[i as usize].dir as usize;
                    match self.kids.pop(&mut self.dirs[d].kids) {
                        Some(c) => {
                            self.free_work.push(FreeWork::Expanded(i));
                            if self.nodes[c as usize].dir == NONE {
                                self.free_slot(c, &mut on_link);
                            } else {
                                self.free_work.push(FreeWork::Expanded(c));
                            }
                        }
                        None => self.free_slot(i, &mut on_link),
                    }
                }
            }
        }
        self.free_work.is_empty()
    }

    fn free_slot(&mut self, i: u32, on_link: &mut impl FnMut(FreedLink)) {
        let n = &self.nodes[i as usize];
        if n.flags.has(NodeFlags::HARDLINK) || n.flags.has(NodeFlags::MEASURED) {
            on_link(FreedLink {
                dev: self.devs[n.dev as usize],
                ino: n.ino,
                node: NodeRef {
                    index: i,
                    generation: n.generation,
                },
                charged: n.flags.has(NodeFlags::CHARGED),
                hardlink: n.flags.has(NodeFlags::HARDLINK),
                measured: n.flags.has(NodeFlags::MEASURED),
            });
        }
        self.classes.remove(&i);
        let n = &mut self.nodes[i as usize];
        self.names_garbage += NameArena::len_of(n.name);
        n.generation = n.generation.wrapping_add(1);
        n.flags = NodeFlags(NodeFlags::FREE);
        n.parent = NONE;
        if n.dir != NONE {
            let d = n.dir;
            n.dir = NONE;
            self.kids.free(&mut self.dirs[d as usize].kids);
            self.free_dirs.push(d);
        }
        self.free_nodes.push(i);
        self.live -= 1;
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
mod tests;
