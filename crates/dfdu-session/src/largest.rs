//! The largest files below a directory, as broot's whale mode: the largest [`TOP`] files
//! anywhere under it in one flat list, with their paths relative to it ([`walk`]), and the
//! Largest view that lists them ([`Largest`], [`act`]).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use dfdu_engine::SizeMode;
use dfdu_tree::{HostPath, Ix, Kind, NodeRef, Tree};

mod view;

pub use view::{
    Info, Intent, Largest, MIN_INTERVAL, Walked, act, active, apply, listed, on_open, read,
    selected, stale,
};

/// The most files the view lists.
pub const TOP: usize = 1000;

/// One of the largest files.
#[derive(Clone, Debug)]
pub struct File {
    pub node: NodeRef,
    /// Relative to the directory walked.
    pub path: HostPath,
    pub size: u64,
    pub dev_ino: (u64, u64),
    pub shared_out: u64,
}

/// The largest [`TOP`] files below `dir` in size mode `mode`, largest first and by path among
/// equals, and how many files there are below it. A walk of the subtree with a heap of at most
/// [`TOP`] files: the cost is one look at each node, and the paths are built only for those
/// kept. `hide_hidden` leaves out hidden files and everything in hidden directories.
pub fn walk(t: &Tree, dir: Ix, mode: SizeMode, hide_hidden: bool) -> (Vec<File>, u64) {
    let mut heap: BinaryHeap<Reverse<(u64, Ix)>> = BinaryHeap::with_capacity(TOP + 1);
    let mut count = 0;
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for c in t.children(d) {
            if hide_hidden && t.name(c).first() == Some(&b'.') {
                continue;
            }
            if t.dir(c).is_some() {
                stack.push(c);
                continue;
            }
            if t.kind(c) != Kind::File {
                continue;
            }
            count += 1;
            let size = mode.of(&t.total(c));
            if heap.len() < TOP {
                heap.push(Reverse((size, c)));
            } else if heap.peek().is_some_and(|Reverse((least, _))| size > *least) {
                heap.pop();
                heap.push(Reverse((size, c)));
            }
        }
    }
    let mut files: Vec<File> = (heap.into_iter())
        .map(|Reverse((size, ix))| File {
            node: t.node_ref(ix),
            path: below(t, dir, ix),
            size,
            dev_ino: t.dev_ino(ix),
            shared_out: t.total(ix).shared_out,
        })
        .collect();
    files.sort_by(|a, b| {
        (b.size.cmp(&a.size)).then_with(|| a.path.components().cmp(b.path.components()))
    });
    (files, count)
}

/// The path of `ix` relative to its ancestor `dir`.
fn below(t: &Tree, dir: Ix, ix: Ix) -> HostPath {
    let mut names = Vec::new();
    let mut at = Some(ix);
    while let Some(i) = at.filter(|&i| i != dir) {
        names.push(t.name(i));
        at = t.parent(i);
    }
    let mut out = HostPath::empty(t.host());
    for n in names.iter().rev() {
        out.push(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use dfdu_engine::test_support::add_file;

    use super::*;
    use crate::test_support::root_model;

    #[test]
    fn the_heap_keeps_only_the_largest() {
        let (mut m, root) = root_model();
        let n = TOP as u64 + 5;
        for i in 1..=n {
            add_file(&mut m.tree, root, &format!("f{i}"), i, 10 + i);
        }
        let (files, count) = walk(&m.tree, root, SizeMode::Disk, false);
        assert_eq!(count, n);
        assert_eq!(files.len(), TOP);
        assert_eq!(files[0].size, n);
        assert_eq!(files[TOP - 1].size, 6);
        assert!(files.windows(2).all(|w| w[0].size >= w[1].size));
    }
}
