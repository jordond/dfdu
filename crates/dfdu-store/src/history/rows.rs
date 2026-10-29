//! The directories of a scan that history keeps, by path relative to the root.

use dfdu_tree::{Ix, Tree};

use super::{KEEP_DEPTH, KEEP_SIZE, MAX_ROWS, SizeMap, Sizes};

/// The directories of the scan below `root` that history keeps: those at most
/// [`KEEP_DEPTH`] levels down, those of at least [`KEEP_SIZE`], and every match of the rules
/// (files included), at most [`MAX_ROWS`], largest first.
#[must_use]
pub fn rows(tree: &Tree, root: Ix) -> SizeMap {
    rows_to(tree, root, KEEP_DEPTH, KEEP_SIZE, MAX_ROWS)
}

/// [`rows`] with other limits: directories at most `depth` levels down or at least `size`
/// bytes, at most `max` of them.
#[must_use]
pub fn rows_to(tree: &Tree, root: Ix, depth: usize, size: u64, max: usize) -> SizeMap {
    let sizes = |ix: Ix| Sizes::of(&tree.total(ix));
    let mut picked: Vec<(Ix, Sizes)> = vec![];
    let mut stack = vec![(root, 0usize)];
    while let Some((ix, d)) = stack.pop() {
        if tree.dir(ix).is_none() {
            continue;
        }
        let s = sizes(ix);
        if d > depth && s.disk < size {
            continue;
        }
        picked.push((ix, s));
        // Only directories: a folder of 10M files stacked 160 MiB of entries to skip.
        stack.extend(
            tree.children(ix)
                .filter(|&c| tree.dir(c).is_some())
                .map(|c| (c, d + 1)),
        );
    }
    // Matches deeper than the walk went, and matched files.
    for (ix, _) in tree.classes() {
        if tree.is_visible(ix) && is_below(tree, ix, root) {
            picked.push((ix, sizes(ix)));
        }
    }
    picked.sort_by_key(|&(ix, s)| (std::cmp::Reverse(s.disk), ix));
    picked.dedup_by_key(|p| p.0);
    let mut out = SizeMap::new();
    for (ix, s) in picked {
        if out.len() >= max {
            break;
        }
        out.entry(rel_path(tree, ix, root)).or_insert(s);
    }
    out
}

fn is_below(tree: &Tree, mut ix: Ix, root: Ix) -> bool {
    loop {
        if ix == root {
            return true;
        }
        match tree.parent(ix) {
            Some(p) => ix = p,
            None => return false,
        }
    }
}

/// The names from below `root` down to `ix`, joined by the platform's separator.
#[must_use]
pub fn rel_path(tree: &Tree, mut ix: Ix, root: Ix) -> Vec<u8> {
    let mut names = vec![];
    while ix != root {
        names.push(tree.name(ix));
        match tree.parent(ix) {
            Some(p) => ix = p,
            None => break,
        }
    }
    names.reverse();
    names.join(&(std::path::MAIN_SEPARATOR as u8))
}

#[cfg(test)]
mod tests;
