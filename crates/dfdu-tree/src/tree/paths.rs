//! Paths of nodes, built from their ancestors' names.

use std::path::PathBuf;

use super::{Ix, SEP, Tree};
use crate::{HostPath, name_os};

impl Tree {
    /// Path of the node on the tree's host: the root's path joined with each name below it,
    /// with that host's separator. Use this for anything shown, sent to an agent or compared
    /// with paths from one; [`Tree::path_buf`] only for a tree of this machine.
    #[must_use]
    pub fn host_path(&self, ix: Ix) -> HostPath {
        let mut parts = Vec::new();
        self.ancestors(ix, &mut parts);
        let mut out = HostPath::empty(self.host);
        for p in parts {
            out.push(self.name(p));
        }
        out
    }

    /// Filesystem path of the node on this machine: the root's path joined with each name below
    /// it. Use this for anything handed to the OS, for a tree of this machine; another host's
    /// tree has [`Tree::host_path`]. [`Tree::path`] is a byte key for ordering and lookups.
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
}

#[cfg(test)]
mod tests;
