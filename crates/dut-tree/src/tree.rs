use crate::{ErrKind, Kind, Meta, Totals};

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

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeFlags(u8);

impl NodeFlags {
    pub const HARDLINK: u8 = 1;
    /// Hard link whose bytes are counted (exactly one per inode and scope).
    pub const CHARGED: u8 = 2;
    /// Root of a detached subtree.
    pub const DETACHED: u8 = 4;
    pub const FREE: u8 = 8;
    /// Root of the visible tree.
    pub const VISIBLE_ROOT: u8 = 16;
    /// In a delete plan that has not finished.
    pub const DELETING: u8 = 32;

    pub fn has(self, f: u8) -> bool {
        self.0 & f != 0
    }
    fn set(&mut self, f: u8, on: bool) {
        if on { self.0 |= f } else { self.0 &= !f }
    }
}

/// Scan state of a directory. Everything but `Pending` is terminal (`data-pipeline.md` I2).
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
    pub fn is_terminal(self) -> bool {
        self != DirState::Pending
    }
}

/// Per-directory record. The scan bookkeeping fields are owned by the applier.
#[derive(Clone, Debug)]
pub struct DirRec {
    pub(crate) children: Vec<u32>,
    pub(crate) total: Totals,
    pub state: DirState,
    /// State taken when `pending` reaches zero, set when the listing closes.
    pub outcome: DirState,
    pub job: u32,
    pub dir_id: u32,
    pub pending: u32,
    pub listing_closed: bool,
    pub reported: bool,
    pub cut: bool,
    pub stalled: bool,
    pub chunks_seen: u32,
    pub close_chunks: Option<u32>,
    pub seen_seq: Vec<u64>,
    pub changed_at: u64,
}

impl DirRec {
    fn new() -> Self {
        DirRec {
            children: Vec::new(),
            total: Totals::default(),
            state: DirState::Pending,
            outcome: DirState::Complete,
            job: 0,
            dir_id: 0,
            pending: 1,
            listing_closed: false,
            reported: false,
            cut: false,
            stalled: false,
            chunks_seen: 0,
            close_chunks: None,
            seen_seq: Vec::new(),
            changed_at: 0,
        }
    }

    /// Records chunk `seq` as seen. Returns false if it was already seen.
    pub fn mark_seq(&mut self, seq: u32) -> bool {
        let (w, b) = ((seq / 64) as usize, seq % 64);
        if self.seen_seq.len() <= w {
            self.seen_seq.resize(w + 1, 0);
        }
        let had = self.seen_seq[w] & (1 << b) != 0;
        self.seen_seq[w] |= 1 << b;
        !had
    }
}

#[derive(Clone, Debug)]
struct Node {
    generation: u32,
    parent: u32,
    name_off: u64,
    name_len: u16,
    kind: Kind,
    flags: NodeFlags,
    apparent: u64,
    alloc: u64,
    mtime: i64,
    dev: u64,
    ino: u64,
    error: Option<ErrKind>,
    dir: u32,
    category: u16,
}

/// A hard-link file reported while freeing a detached subtree, so the caller can update its
/// link table.
#[derive(Copy, Clone, Debug)]
pub struct FreedLink {
    pub dev: u64,
    pub ino: u64,
    pub node: NodeRef,
    pub charged: bool,
}

#[derive(Copy, Clone, Debug)]
enum FreeWork {
    Node(u32),
    Expanded(u32),
}

/// The node store. See the crate docs for who may mutate it.
#[derive(Default)]
pub struct Tree {
    nodes: Vec<Node>,
    dirs: Vec<DirRec>,
    free_nodes: Vec<u32>,
    free_dirs: Vec<u32>,
    names: Vec<u8>,
    names_garbage: u64,
    roots: Vec<u32>,
    version: u64,
    free_work: Vec<FreeWork>,
    live: u64,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- reading -------------------------------------------------------------------------

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn live_nodes(&self) -> u64 {
        self.live
    }

    pub fn name_bytes(&self) -> (u64, u64) {
        (self.names.len() as u64, self.names_garbage)
    }

    pub fn roots(&self) -> impl Iterator<Item = Ix> + '_ {
        self.roots.iter().map(|&i| Ix(i))
    }

    pub fn node_ref(&self, ix: Ix) -> NodeRef {
        NodeRef {
            index: ix.0,
            generation: self.nodes[ix.0 as usize].generation,
        }
    }

    /// Resolves a reference to a live, attached node (visible or in a shadow subtree).
    pub fn resolve(&self, r: NodeRef) -> Option<Ix> {
        let n = self.nodes.get(r.index as usize)?;
        if n.generation != r.generation || n.flags.has(NodeFlags::FREE) {
            return None;
        }
        self.top(Ix(r.index)).map(|_| Ix(r.index))
    }

    /// Resolves a reference to a node reachable from a visible root.
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

    pub fn is_attached(&self, ix: Ix) -> bool {
        self.top(ix).is_some()
    }

    pub fn is_visible(&self, ix: Ix) -> bool {
        self.top(ix)
            .is_some_and(|t| self.nodes[t.0 as usize].flags.has(NodeFlags::VISIBLE_ROOT))
    }

    pub fn parent(&self, ix: Ix) -> Option<Ix> {
        let p = self.nodes[ix.0 as usize].parent;
        (p != NONE).then_some(Ix(p))
    }

    pub fn name(&self, ix: Ix) -> &[u8] {
        let n = &self.nodes[ix.0 as usize];
        &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize]
    }

    pub fn kind(&self, ix: Ix) -> Kind {
        self.nodes[ix.0 as usize].kind
    }

    pub fn flags(&self, ix: Ix) -> NodeFlags {
        self.nodes[ix.0 as usize].flags
    }

    /// Size fields as scanned: `(apparent, alloc)`, before hard-link charging.
    pub fn raw_size(&self, ix: Ix) -> (u64, u64) {
        let n = &self.nodes[ix.0 as usize];
        (n.apparent, n.alloc)
    }

    pub fn mtime(&self, ix: Ix) -> i64 {
        self.nodes[ix.0 as usize].mtime
    }

    pub fn dev_ino(&self, ix: Ix) -> (u64, u64) {
        let n = &self.nodes[ix.0 as usize];
        (n.dev, n.ino)
    }

    pub fn error(&self, ix: Ix) -> Option<ErrKind> {
        self.nodes[ix.0 as usize].error
    }

    pub fn category(&self, ix: Ix) -> u16 {
        self.nodes[ix.0 as usize].category
    }

    pub fn dir(&self, ix: Ix) -> Option<&DirRec> {
        let d = self.nodes[ix.0 as usize].dir;
        (d != NONE).then(|| &self.dirs[d as usize])
    }

    pub fn dir_mut(&mut self, ix: Ix) -> Option<&mut DirRec> {
        let d = self.nodes[ix.0 as usize].dir;
        (d != NONE).then(|| &mut self.dirs[d as usize])
    }

    pub fn children(&self, ix: Ix) -> impl ExactSizeIterator<Item = Ix> + '_ {
        self.dir(ix)
            .map_or(&[][..], |d| &d.children[..])
            .iter()
            .map(|&c| Ix(c))
    }

    /// What this node contributes by itself (one item, its own error, its own bytes unless it
    /// is an uncharged hard link).
    pub fn own(&self, ix: Ix) -> Totals {
        let n = &self.nodes[ix.0 as usize];
        let counted = !n.flags.has(NodeFlags::HARDLINK) || n.flags.has(NodeFlags::CHARGED);
        Totals {
            apparent: if counted { n.apparent } else { 0 },
            alloc: if counted { n.alloc } else { 0 },
            items: 1,
            errors: n.error.is_some() as u64,
        }
    }

    /// Totals of the node and everything below it.
    pub fn total(&self, ix: Ix) -> Totals {
        match self.dir(ix) {
            Some(d) => d.total,
            None => self.own(ix),
        }
    }

    /// Path from the topmost ancestor, components joined with [`SEP`].
    pub fn path(&self, ix: Ix) -> Vec<u8> {
        let mut parts = vec![ix];
        let mut cur = ix;
        while let Some(p) = self.parent(cur) {
            parts.push(p);
            cur = p;
        }
        let mut out = Vec::new();
        for (i, p) in parts.iter().rev().enumerate() {
            let name = self.name(*p);
            if i > 0 && out.last() != Some(&SEP) {
                out.push(SEP);
            }
            out.extend_from_slice(name);
        }
        out
    }

    // ---- building --------------------------------------------------------------------------

    fn alloc_node(&mut self, parent: u32, name: &[u8], meta: &Meta, flags: u8) -> u32 {
        let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
        let name = &name[..name_len as usize];
        let name_off = self.names.len() as u64;
        self.names.extend_from_slice(name);
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
            generation,
            parent,
            name_off,
            name_len,
            kind: meta.kind,
            flags: f,
            apparent: meta.apparent,
            alloc: meta.alloc,
            mtime: meta.mtime,
            dev: meta.dev,
            ino: meta.ino,
            error: None,
            dir: NONE,
            category: 0,
        };
        self.live += 1;
        match self.free_nodes.pop() {
            Some(i) => {
                self.nodes[i as usize] = node;
                i
            }
            None => {
                self.nodes.push(node);
                (self.nodes.len() - 1) as u32
            }
        }
    }

    fn alloc_dir(&mut self, node: u32) {
        let rec = DirRec::new();
        let d = match self.free_dirs.pop() {
            Some(d) => {
                self.dirs[d as usize] = rec;
                d
            }
            None => {
                self.dirs.push(rec);
                (self.dirs.len() - 1) as u32
            }
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
        let i = self.alloc_node(parent.0, name, meta, 0);
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
        self.nodes[i as usize].error = error;
        self.push_child(parent, i);
        Ix(i)
    }

    fn push_child(&mut self, parent: Ix, child: u32) {
        let d = self.nodes[parent.0 as usize].dir;
        assert!(d != NONE, "child added to a non-directory");
        self.dirs[d as usize].children.push(child);
    }

    fn bump(&mut self) -> u64 {
        self.version += 1;
        self.version
    }

    /// Adds `delta` to `dir` and every ancestor, and marks them changed.
    pub fn propagate_add(&mut self, dir: Ix, delta: &Totals) {
        let v = self.bump();
        let mut i = dir.0;
        while i != NONE {
            let n = &self.nodes[i as usize];
            let (d, p) = (n.dir, n.parent);
            let rec = &mut self.dirs[d as usize];
            rec.total.add(delta);
            rec.changed_at = v;
            i = p;
        }
    }

    /// Subtracts `delta` from `dir` and every ancestor, and marks them changed.
    pub fn propagate_sub(&mut self, dir: Ix, delta: &Totals) {
        let v = self.bump();
        let mut i = dir.0;
        while i != NONE {
            let n = &self.nodes[i as usize];
            let (d, p) = (n.dir, n.parent);
            let rec = &mut self.dirs[d as usize];
            rec.total.sub(delta);
            rec.changed_at = v;
            i = p;
        }
    }

    /// Records an error on the node itself (a directory that could not be read, or an entry
    /// that could not be stat'ed) and counts it in the ancestors.
    pub fn set_error(&mut self, ix: Ix, err: ErrKind) {
        let had = self.nodes[ix.0 as usize].error.replace(err).is_some();
        if !had {
            let one = Totals {
                errors: 1,
                ..Totals::default()
            };
            match self.dir(ix).is_some() {
                true => self.propagate_add(ix, &one),
                false => {
                    if let Some(p) = self.parent(ix) {
                        self.propagate_add(p, &one)
                    }
                }
            }
        }
    }

    /// Changes whether a hard link's bytes are counted, and adjusts the ancestors.
    pub fn set_charged(&mut self, ix: Ix, charged: bool) {
        let n = &self.nodes[ix.0 as usize];
        if !n.flags.has(NodeFlags::HARDLINK) || n.flags.has(NodeFlags::CHARGED) == charged {
            return;
        }
        let delta = Totals {
            apparent: n.apparent,
            alloc: n.alloc,
            items: 0,
            errors: 0,
        };
        self.nodes[ix.0 as usize]
            .flags
            .set(NodeFlags::CHARGED, charged);
        if let Some(p) = self.parent(ix) {
            if charged {
                self.propagate_add(p, &delta)
            } else {
                self.propagate_sub(p, &delta)
            }
        }
    }

    /// Replaces the node's own stat (a job root's stat arrives in its first chunk) and adjusts
    /// the totals of the node and its ancestors.
    pub fn update_meta(&mut self, ix: Ix, meta: &Meta) {
        let old = self.own(ix);
        let n = &mut self.nodes[ix.0 as usize];
        n.apparent = meta.apparent;
        n.alloc = meta.alloc;
        n.mtime = meta.mtime;
        n.dev = meta.dev;
        n.ino = meta.ino;
        let new = self.own(ix);
        let target = if self.dir(ix).is_some() {
            Some(ix)
        } else {
            self.parent(ix)
        };
        if let Some(t) = target {
            self.propagate_add(t, &new);
            self.propagate_sub(t, &old);
        }
    }

    pub fn set_deleting(&mut self, ix: Ix, on: bool) {
        self.nodes[ix.0 as usize].flags.set(NodeFlags::DELETING, on);
        self.bump();
    }

    pub fn set_category(&mut self, ix: Ix, category: u16) {
        self.nodes[ix.0 as usize].category = category;
        self.bump();
    }

    /// Marks a directory changed without changing totals (state changes the UI should see).
    pub fn touch(&mut self, ix: Ix) {
        let v = self.bump();
        if let Some(d) = self.dir_mut(ix) {
            d.changed_at = v;
        }
    }

    // ---- detaching and freeing ---------------------------------------------------------------

    /// Unlinks `ix` from its parent (or from the visible roots), subtracts its totals from the
    /// ancestors and queues the subtree for freeing. References into it stop resolving at once.
    pub fn detach(&mut self, ix: Ix) {
        let total = self.total(ix);
        let parent = self.nodes[ix.0 as usize].parent;
        if parent != NONE {
            self.remove_child(Ix(parent), ix.0);
            self.propagate_sub(Ix(parent), &total);
        } else {
            self.roots.retain(|&r| r != ix.0);
            self.bump();
        }
        let n = &mut self.nodes[ix.0 as usize];
        n.parent = NONE;
        n.flags.set(NodeFlags::DETACHED, true);
        n.flags.set(NodeFlags::VISIBLE_ROOT, false);
        self.free_work.push(FreeWork::Node(ix.0));
    }

    /// Puts the non-visible root `new` in the place of `old` (rescan swap) and detaches `old`.
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
        if parent != NONE {
            let d = self.nodes[parent as usize].dir as usize;
            let pos = self.dirs[d]
                .children
                .iter()
                .position(|&c| c == old.0)
                .expect("child");
            self.dirs[d].children[pos] = new.0;
            self.nodes[new.0 as usize].parent = parent;
            self.propagate_sub(Ix(parent), &old_total);
            self.propagate_add(Ix(parent), &new_total);
        } else {
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
        }
        let n = &mut self.nodes[old.0 as usize];
        n.parent = NONE;
        n.flags.set(NodeFlags::DETACHED, true);
        n.flags.set(NodeFlags::VISIBLE_ROOT, false);
        self.free_work.push(FreeWork::Node(old.0));
    }

    fn remove_child(&mut self, parent: Ix, child: u32) {
        let d = self.nodes[parent.0 as usize].dir as usize;
        let ch = &mut self.dirs[d].children;
        if let Some(pos) = ch.iter().position(|&c| c == child) {
            ch.remove(pos);
        }
    }

    pub fn has_free_work(&self) -> bool {
        !self.free_work.is_empty()
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
                    match self.dirs[d].children.pop() {
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
        if n.flags.has(NodeFlags::HARDLINK) {
            on_link(FreedLink {
                dev: n.dev,
                ino: n.ino,
                node: NodeRef {
                    index: i,
                    generation: n.generation,
                },
                charged: n.flags.has(NodeFlags::CHARGED),
            });
        }
        let n = &mut self.nodes[i as usize];
        self.names_garbage += n.name_len as u64;
        n.generation = n.generation.wrapping_add(1);
        n.flags = NodeFlags(NodeFlags::FREE);
        n.parent = NONE;
        if n.dir != NONE {
            let d = n.dir;
            n.dir = NONE;
            self.dirs[d as usize].children = Vec::new();
            self.free_dirs.push(d);
        }
        self.free_nodes.push(i);
        self.live -= 1;
    }

    // ---- crate-internal access for the invariant checker ------------------------------------

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
mod tests {
    use super::*;

    fn dir_meta() -> Meta {
        Meta {
            kind: Kind::Dir,
            apparent: 10,
            alloc: 4096,
            mtime: 0,
            dev: 1,
            ino: 1,
            nlink: 2,
        }
    }
    fn file_meta(size: u64, ino: u64, nlink: u32) -> Meta {
        Meta {
            kind: Kind::File,
            apparent: size,
            alloc: size,
            mtime: 0,
            dev: 1,
            ino,
            nlink,
        }
    }

    fn add_file(t: &mut Tree, p: Ix, name: &str, size: u64) -> Ix {
        let f = t.add_file(p, name.as_bytes(), &file_meta(size, size, 1), false, None);
        let own = t.own(f);
        t.propagate_add(p, &own);
        f
    }

    fn add_dir(t: &mut Tree, p: Ix, name: &str) -> Ix {
        let d = t.add_dir(p, name.as_bytes(), &dir_meta());
        let own = t.own(d);
        t.propagate_add(p, &own);
        d
    }

    #[test]
    fn totals_propagate_and_detach_subtracts() {
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &dir_meta(), true);
        let a = add_dir(&mut t, root, "a");
        add_file(&mut t, a, "f1", 100);
        add_file(&mut t, a, "f2", 50);
        add_file(&mut t, root, "g", 7);
        assert_eq!(t.total(root).apparent, 10 + 10 + 100 + 50 + 7);
        assert_eq!(t.total(root).items, 5);
        t.check(&[]).unwrap();

        let a_ref = t.node_ref(a);
        t.detach(a);
        assert_eq!(t.total(root).apparent, 17);
        assert!(
            t.resolve(a_ref).is_none(),
            "detached nodes stop resolving immediately"
        );
        t.check(&[]).unwrap();
        while !t.free_step(1, |_| {}) {}
        assert_eq!(t.live_nodes(), 2);
    }

    #[test]
    fn freed_slot_reuse_bumps_generation() {
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &dir_meta(), true);
        let f = add_file(&mut t, root, "f", 1);
        let old = t.node_ref(f);
        t.detach(f);
        t.free_step(usize::MAX, |_| {});
        let g = add_file(&mut t, root, "g", 2);
        assert_eq!(g.0, f.0, "slot reused");
        assert!(t.resolve(old).is_none());
        assert!(t.resolve(t.node_ref(g)).is_some());
    }

    #[test]
    fn freeing_is_bottom_up() {
        // A deep subtree freed one unit at a time never leaves a live node under a freed slot.
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &dir_meta(), true);
        let top = add_dir(&mut t, root, "top");
        let mut cur = top;
        let mut refs = vec![];
        for i in 0..20 {
            cur = add_dir(&mut t, cur, &format!("d{i}"));
            refs.push(t.node_ref(cur));
            add_file(&mut t, cur, "x", 3);
        }
        t.detach(top);
        loop {
            let done = t.free_step(1, |_| {});
            // Allocate into freed slots between steps, as the applier would.
            if let Some(&slot) = t.free_nodes.last() {
                let _ = slot;
                let n = add_file(&mut t, root, "new", 1);
                for r in &refs {
                    if let Some(ix) = t.resolve(*r) {
                        assert!(!t.is_visible(ix), "a freed subtree node became visible");
                    }
                }
                let _ = n;
            }
            if done {
                break;
            }
        }
        t.check(&[]).unwrap();
    }

    #[test]
    fn replace_swaps_totals() {
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &dir_meta(), true);
        let a = add_dir(&mut t, root, "a");
        add_file(&mut t, a, "old", 100);
        let shadow = t.new_root(b"a", &dir_meta(), false);
        add_file(&mut t, shadow, "new", 30);
        t.check(&[shadow]).unwrap();
        t.replace(a, shadow);
        assert_eq!(t.total(root).apparent, 10 + 10 + 30);
        assert!(t.is_visible(shadow));
        t.check(&[]).unwrap();
    }

    #[test]
    fn hardlink_charging() {
        let mut t = Tree::new();
        let root = t.new_root(b"/r", &dir_meta(), true);
        let m = file_meta(100, 42, 2);
        let a = t.add_file(root, b"a", &m, true, None);
        let b = t.add_file(root, b"b", &m, false, None);
        let delta = {
            let mut d = t.own(a);
            d.add(&t.own(b));
            d
        };
        t.propagate_add(root, &delta);
        assert_eq!(t.total(root).apparent, 10 + 100);
        t.set_charged(a, false);
        t.set_charged(b, true);
        assert_eq!(t.total(root).apparent, 10 + 100);
        t.check(&[]).unwrap();
    }
}
