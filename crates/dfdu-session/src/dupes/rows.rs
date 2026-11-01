//! The view's rows and what a tick takes: the groups with two files left, each open one
//! followed by its files, and the mark of a file.

use dfdu_dupes::{Candidate, Group};
use dfdu_tree::{HostPath, Ident, Kind, NodeRef, Target};

use super::{found, live};
use crate::marks;
use crate::state::SessionState;

/// A row of the view: a group, or a file of one by its index in the group.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Row {
    Group(usize),
    File(usize, usize),
}

/// Whether group `g` shows its files: by default unless it is clones, which free nothing.
pub fn open(s: &SessionState, g: usize, group: &Group) -> bool {
    !group.all_clones() ^ s.dupes.toggled.contains(&g)
}

/// The groups with at least two files left, clones last; each open one followed by its files.
pub fn rows(s: &SessionState) -> Vec<Row> {
    let Some(f) = found(s) else {
        return Vec::new();
    };
    let nodes = &s.extras.dupes;
    let mut order: Vec<usize> = (0..f.groups.len()).collect();
    order.sort_by_key(|&g| f.groups[g].all_clones());
    let mut out = Vec::new();
    for g in order {
        let group = &f.groups[g];
        let left: Vec<usize> = (0..group.files.len())
            .filter(|&i| live(nodes, &group.files[i]).is_some())
            .collect();
        if left.len() < 2 {
            continue;
        }
        out.push(Row::Group(g));
        if open(s, g, group) {
            out.extend(left.into_iter().map(|i| Row::File(g, i)));
        }
    }
    out
}

/// The mark of file `c`, at `node` in the tree.
pub fn mark_of(c: &Candidate, node: NodeRef) -> marks::Item {
    marks::Item {
        target: Target {
            node: Some(node),
            path: HostPath::local(&c.path),
            ident: Some(Ident {
                dev_ino: c.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: c.freed(),
        items: 1,
        shared_out: 0,
        scanning: false,
        inside: false,
    }
}

/// Marks every copy but the one kept in each group that frees something
/// ([`super::suggested`]); returns how many were newly marked.
pub fn mark_all(s: &mut SessionState) -> usize {
    let Some(f) = found(s) else {
        return 0;
    };
    let picks: Vec<(Candidate, NodeRef)> = super::suggested(f, s.dupes.keep, &s.extras.dupes)
        .into_iter()
        .filter(|(_, n)| !s.marks.contains(*n))
        .collect();
    for (c, n) in &picks {
        marks::set(s, mark_of(c, *n), true);
    }
    picks.len()
}
