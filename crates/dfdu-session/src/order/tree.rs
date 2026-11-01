//! Row order in tree mode: siblings are sorted within each parent, and every row is followed
//! by its children (a preorder of the expanded tree). The rules of the flat list apply to each
//! group of siblings: an exact sort when asked for, the automatic sort by bucket with the
//! previous position as tie-break, and between sorts the previous positions, with siblings
//! that appeared after their known siblings. A group none of which had a position before (a
//! directory just expanded) gets an exact sort. Fold rows come last among their siblings.

use super::{SortSpec, ranked};
use crate::snapshot::EntryData;

/// How each group of siblings is ordered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Siblings {
    /// An exact sort.
    Exact,
    /// The automatic sort: sizes and counts by bucket, previous position as tie-break.
    Auto,
    /// No sort: previous positions, new siblings after them in arrival order.
    Keep,
}

/// The rows of a tree snapshot sorted by `spec`: every entry, siblings sorted exactly, in
/// preorder.
pub fn tree_order(entries: &[EntryData], spec: SortSpec) -> Vec<usize> {
    tree_rows(entries, |_, _| true, spec, &[], Siblings::Exact)
}

/// The entries `visible` passes whose ancestors it all passes too, in preorder with siblings
/// ordered as `how` says. `prev[i]`: the previous position of entry `i` (missing: none), used
/// by [`Siblings::Auto`] and [`Siblings::Keep`].
pub fn tree_rows(
    entries: &[EntryData],
    visible: impl Fn(usize, &EntryData) -> bool,
    spec: SortSpec,
    prev: &[u32],
    how: Siblings,
) -> Vec<usize> {
    let n = entries.len();
    let shown: Vec<bool> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| visible(i, e))
        .collect();
    let parent = |i: usize| {
        let p = entries[i].parent;
        (p != u32::MAX).then_some(p as usize)
    };
    // The children of each entry, as runs of one array (`start[i]..start[i + 1]`).
    let mut top = Vec::new();
    let mut start = vec![0usize; n + 1];
    for i in (0..n).filter(|&i| shown[i]) {
        match parent(i) {
            None => top.push(i),
            Some(p) if p < n && shown[p] => start[p + 1] += 1,
            Some(_) => {}
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut kids = vec![0usize; start[n]];
    let mut fill = start.clone();
    for i in (0..n).filter(|&i| shown[i]) {
        if let Some(p) = parent(i).filter(|&p| p < n && shown[p]) {
            kids[fill[p]] = i;
            fill[p] += 1;
        }
    }
    let top = siblings(entries, &top, spec, prev, how);
    for p in 0..n {
        let run = start[p]..start[p + 1];
        if run.len() > 1 {
            let sorted = siblings(entries, &kids[run.clone()], spec, prev, how);
            kids[run].copy_from_slice(&sorted);
        }
    }

    let mut rows = Vec::with_capacity(top.len() + kids.len());
    let mut stack = vec![top.iter()];
    while let Some(it) = stack.last_mut() {
        match it.next() {
            Some(&i) => {
                rows.push(i);
                let run = &kids[start[i]..start[i + 1]];
                if !run.is_empty() {
                    stack.push(run.iter());
                }
            }
            None => {
                stack.pop();
            }
        }
    }
    rows
}

/// One group of siblings in order, fold rows last.
fn siblings(
    entries: &[EntryData],
    group: &[usize],
    spec: SortSpec,
    prev: &[u32],
    how: Siblings,
) -> Vec<usize> {
    let real = group.iter().copied().filter(|&i| entries[i].fold == 0);
    let pos = |i: usize| prev.get(i).copied().unwrap_or(u32::MAX);
    let mut out = match how {
        Siblings::Keep if group.iter().any(|&i| pos(i) != u32::MAX) => {
            let mut v: Vec<usize> = real.collect();
            v.sort_by_key(|&i| (pos(i), i));
            v
        }
        Siblings::Auto => ranked(entries, real, spec, prev, usize::MAX),
        Siblings::Exact | Siblings::Keep => ranked(entries, real, spec, &[], usize::MAX),
    };
    out.extend(group.iter().copied().filter(|&i| entries[i].fold > 0));
    out
}

#[cfg(test)]
mod tests {
    use super::super::SortKey;
    use super::*;
    use crate::test_support::sample;

    /// The names of `rows` of `es`.
    fn names(es: &[EntryData], rows: &[usize]) -> Vec<String> {
        rows.iter()
            .map(|&i| String::from_utf8_lossy(&es[i].name).into_owned())
            .collect()
    }

    const ALL: [&str; 9] = ["a", "x", "y", "p", "q", "z", "b", "only", "deep"];

    #[test]
    fn siblings_sort_within_their_parent_in_preorder() {
        let es = sample();
        assert_eq!(names(&es, &tree_order(&es, SortSpec::default())), ALL);
        let z_to_a = SortSpec {
            key: SortKey::Name,
            desc: true,
        };
        assert_eq!(
            names(&es, &tree_order(&es, z_to_a)),
            ["b", "only", "deep", "a", "z", "y", "q", "p", "x"]
        );
        let mut es = es;
        es[5].size = 60;
        assert_eq!(
            names(&es, &tree_order(&es, SortSpec::default())),
            ["a", "z", "x", "y", "p", "q", "b", "only", "deep"]
        );
    }

    #[test]
    fn a_row_shows_only_with_every_row_above_it() {
        let es = sample();
        let hide_y = |i: usize, _: &EntryData| i != 2;
        let rows = tree_rows(&es, hide_y, SortSpec::default(), &[], Siblings::Exact);
        assert_eq!(names(&es, &rows), ["a", "x", "z", "b", "only", "deep"]);
    }

    #[test]
    fn kept_siblings_hold_their_places_and_new_ones_follow() {
        let es = sample();
        // Below `a`, z was first and x second before; y is new.
        let mut prev = vec![u32::MAX; es.len()];
        (prev[0], prev[5], prev[1], prev[6]) = (0, 1, 2, 3);
        let rows = tree_rows(&es, |_, _| true, SortSpec::default(), &prev, Siblings::Keep);
        assert_eq!(
            names(&es, &rows),
            ["a", "z", "x", "y", "p", "q", "b", "only", "deep"]
        );
        // A group none of which had a place gets an exact sort: p before q.
        let exact = tree_rows(&es, |_, _| true, SortSpec::default(), &[], Siblings::Keep);
        assert_eq!(names(&es, &exact), ALL);
    }

    #[test]
    fn fold_rows_come_last_among_their_siblings() {
        let mut es = sample();
        // `x`, the largest below `a`, stands for a fold row.
        es[1].fold = 4;
        let rows = tree_order(&es, SortSpec::default());
        assert_eq!(
            names(&es, &rows),
            ["a", "y", "p", "q", "z", "x", "b", "only", "deep"]
        );
    }
}
