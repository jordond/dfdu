//! Tree mode: the state, the helpers on the rows of a tree snapshot and what the keys do (`act`)
//! are in [`dfdu_session::tree`], re-exported here; the guide drawn before each name is here.

use std::collections::HashSet;

pub(crate) use dfdu_session::tree::*;

use crate::model::snapshot::EntryData;

/// Which entries are the last of their parent's children in `rows` (display order), indexed
/// by entry, for [`guide`].
pub fn last_children(entries: &[EntryData], rows: &[usize]) -> Vec<bool> {
    let mut last = vec![false; entries.len()];
    let mut seen: HashSet<u32> = HashSet::new();
    for &i in rows.iter().rev() {
        if seen.insert(entries[i].parent) {
            last[i] = true;
        }
    }
    last
}

/// The guide drawn before the name of entry `i`: nothing at the top, then for each ancestor
/// below the top `│ ` while it has siblings after it (or blank), then `├─`, or `└─` for the
/// last child. `last` comes from [`last_children`].
pub fn guide(entries: &[EntryData], last: &[bool], i: usize) -> String {
    if parent(entries, i).is_none() {
        return String::new();
    }
    let mut parts = vec![if last[i] { "└─" } else { "├─" }];
    let mut cur = i;
    while let Some(p) = parent(entries, cur) {
        if parent(entries, p).is_none() {
            break;
        }
        parts.push(if last[p] { "  " } else { "│ " });
        cur = p;
    }
    parts.iter().rev().copied().collect()
}

#[cfg(test)]
pub(crate) mod tests;
