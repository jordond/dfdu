//! Adding and changing nodes, and keeping their ancestors' totals. Taking them out again is
//! in [`detach`].

mod detach;

pub(super) use detach::FreeWork;
pub use detach::FreedLink;

use super::{Ix, NONE, Node, NodeFlags, Tree};
use crate::{DirRec, ErrKind, Meta, Totals};

impl Tree {
    fn alloc_node(&mut self, parent: u32, name: &[u8], meta: &Meta, flags: NodeFlags) -> u32 {
        // Names are never truncated: a cut name would not convert back (`name_os`). Entry names
        // are at most a few hundred bytes; root paths can be longer, up to 16 MiB.
        let name = self.names.push(name);
        let dev = self.dev_index(meta.dev);
        let mut f = flags;
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
        let flags = if visible {
            NodeFlags::VISIBLE_ROOT
        } else {
            NodeFlags::default()
        };
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
        self.add_dir_flagged(parent, name, meta, NodeFlags::default())
    }

    /// [`Tree::add_dir`] with flags set from the start (`MOUNT_ROOT`, `DUP_MOUNT`), so they
    /// are part of the directory's own totals.
    pub fn add_dir_flagged(
        &mut self,
        parent: Ix,
        name: &[u8],
        meta: &Meta,
        flags: NodeFlags,
    ) -> Ix {
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
            NodeFlags::default()
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
    pub fn set_flag(&mut self, ix: Ix, flag: NodeFlags, on: bool) {
        self.restate(ix, |t| t.nodes[ix.0 as usize].flags.set(flag, on));
    }

    /// Changes a node and moves the difference in its own totals into its ancestors' totals.
    pub(super) fn restate(&mut self, ix: Ix, f: impl FnOnce(&mut Self)) {
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
}

#[cfg(test)]
mod tests;
