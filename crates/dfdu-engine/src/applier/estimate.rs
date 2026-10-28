//! What deleting a set of nodes would free.

use std::collections::{HashMap, HashSet};

use dfdu_scan::Backend;
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef};

use super::{Applier, Estimate, Model};
use crate::extents::Inode;

impl<B: Backend> Applier<B> {
    /// What deleting `items` would free. Walks them, so it is linear in their size.
    pub fn estimate(&self, m: &Model, items: &[NodeRef], id: u64) -> Estimate {
        let tree = &m.tree;
        let all: HashSet<Ix> = items
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        let tops: Vec<Ix> = all
            .iter()
            .copied()
            .filter(|&ix| {
                let mut a = ix;
                while let Some(p) = tree.parent(a) {
                    if all.contains(&p) {
                        return false;
                    }
                    a = p;
                }
                true
            })
            .collect();
        let mut e = Estimate {
            id,
            items: tops.len(),
            // APFS clones share data without a sign the scan can read.
            may_free_less: cfg!(target_os = "macos"),
            ..Default::default()
        };
        // Hard-linked inodes in the targets: links seen, and the charged link's bytes if it is
        // among them.
        let mut linked: HashMap<Inode, (u32, u64)> = HashMap::new();
        for &t in &tops {
            let tot = tree.total(t);
            e.bytes += tot.alloc;
            e.shared_out += tot.shared_out;
            e.nodes += tot.items;
            e.may_free_less |= self.extent_devs.contains(&tree.dev_ino(t).0) && !self.measure;
            let mut stack = vec![t];
            while let Some(ix) = stack.pop() {
                if let Some(d) = tree.dir(ix) {
                    e.scanning |= d.state == DirState::Pending || d.cut;
                    stack.extend(tree.children(ix));
                    continue;
                }
                let f = tree.flags(ix);
                if f.has(NodeFlags::HARDLINK) {
                    let l = linked.entry(tree.dev_ino(ix)).or_default();
                    l.0 += 1;
                    if f.has(NodeFlags::CHARGED) {
                        l.1 = tree.own(ix).alloc;
                    }
                }
            }
        }
        for (key, (seen, bytes)) in linked {
            if seen < self.links.nlink(key) {
                e.linked_out += bytes;
            }
        }
        e.bytes = e.bytes.saturating_sub(e.shared_out + e.linked_out);
        e
    }
}
