//! Adding, changing, detaching and freeing nodes, and keeping their ancestors' totals.

use std::collections::HashMap;

use super::{Ix, NONE, Node, NodeFlags, NodeRef, Tree};
use crate::pages::NameArena;
use crate::{DirRec, ErrKind, Meta, Totals};

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
pub(super) enum FreeWork {
    Node(u32),
    Expanded(u32),
}

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

    /// Moves the subtree at each `kept` of `pairs` into the place of the directory
    /// `placeholder`, under the placeholder's name, and detaches the placeholder: a resume
    /// keeps a loaded directory where its rescan found it again. The ancestors on both sides
    /// keep their totals. Each parent's children are gone through once, for all the pairs.
    ///
    /// # Panics
    ///
    /// A `kept` or `placeholder` is a root, or a `placeholder` is not a child of its parent.
    pub fn graft(&mut self, pairs: &[(Ix, Ix)]) {
        // By the placeholders' parent, then the placeholder: each parent's are a run, looked up
        // in as its children are gone through.
        let mut to: Vec<(u32, u32, u32)> = pairs
            .iter()
            .map(|&(p, k)| (self.nodes[p.0 as usize].parent, p.0, k.0))
            .collect();
        to.sort_unstable();
        // The totals that come to each run's parent and leave it, and those that leave each
        // parent of the kept directories, siblings together.
        let mut runs = Vec::new();
        let mut from: HashMap<u32, Totals> = HashMap::new();
        let mut last: Option<(u32, Totals)> = None;
        for run in to.chunk_by(|a, b| a.0 == b.0) {
            assert!(run[0].0 != NONE, "neither is a root");
            let (mut sum, mut old) = (Totals::default(), Totals::default());
            for &(_, p, k) in run {
                let f = self.nodes[k as usize].parent;
                assert!(f != NONE, "neither is a root");
                let total = self.total(Ix(k));
                sum.add(&total);
                old.add(&self.total(Ix(p)));
                match &mut last {
                    Some((q, left)) if *q == f => left.add(&total),
                    _ => {
                        if let Some((q, left)) = last.replace((f, total)) {
                            from.entry(q).or_default().add(&left);
                        }
                    }
                }
            }
            runs.push((sum, old));
        }
        if let Some((q, left)) = last {
            from.entry(q).or_default().add(&left);
        }
        // The kept directories leave their parents, which tell them by their parent.
        for &(_, _, k) in &to {
            self.nodes[k as usize].parent = NONE;
        }
        for (f, left) in from {
            let d = self.nodes[f as usize].dir as usize;
            let nodes = &self.nodes;
            self.kids
                .retain(&mut self.dirs[d].kids, |c| nodes[c as usize].parent == f);
            self.propagate_sub(Ix(f), &left);
        }
        for (run, (sum, old)) in to.chunk_by(|a, b| a.0 == b.0).zip(runs) {
            let t = run[0].0;
            let d = self.nodes[t as usize].dir as usize;
            // Placeholders are listed in the order they were added: the next is tried first.
            let (mut next, mut found) = (0, 0);
            self.kids.replace_each(&mut self.dirs[d].kids, |c| {
                let at = match run.get(next) {
                    Some(e) if e.1 == c => Some(next),
                    _ => run.binary_search_by_key(&c, |e| e.1).ok(),
                };
                at.map_or(c, |i| {
                    (next, found) = (i + 1, found + 1);
                    run[i].2
                })
            });
            assert_eq!(found, run.len(), "child");
            self.propagate_sub(Ix(t), &old);
            self.propagate_add(Ix(t), &sum);
        }
        for &(t, p, k) in &to {
            let names = (self.nodes[k as usize].name, self.nodes[p as usize].name);
            let n = &mut self.nodes[k as usize];
            n.parent = t;
            n.name = names.1;
            let n = &mut self.nodes[p as usize];
            n.name = names.0;
            n.parent = NONE;
            n.flags.set(NodeFlags::DETACHED, true);
            self.free_work.push(FreeWork::Node(p));
        }
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
        self.extents.remove(&i);
        let n = &mut self.nodes[i as usize];
        self.names_garbage += NameArena::len_of(n.name);
        n.generation = n.generation.wrapping_add(1);
        n.flags = NodeFlags::FREE;
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
}

#[cfg(test)]
mod tests;
