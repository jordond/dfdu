//! What the keys do in tree mode: [`toggle`] turns it on and off, [`right`] expands, [`left`]
//! collapses, [`chain`] expands the chain of largest entries and [`cycle_fold`] changes the
//! fold threshold. A click on a row's expander expands or collapses its directory
//! ([`toggle_row`]). The treemap shows the directory of the selected row ([`treemap_dir`]).

use super::{TreeState, parent, path_of, target_of};
use crate::state::{Moved, SessionState};

/// Turns tree mode on or off. Turning it off forgets what was expanded.
pub fn toggle(s: &mut SessionState) {
    s.tree = match s.tree {
        Some(_) => None,
        None => Some(TreeState::default()),
    };
    s.request_sort();
}

/// The fold threshold as the header and the hints write it; `None` when nothing folds.
pub fn fold_label(ratio: f64) -> Option<&'static str> {
    if ratio <= 0.0 {
        None
    } else if ratio >= 0.01 {
        Some("fold < 1%")
    } else {
        Some("fold < 0.1%")
    }
}

/// The selected row and its entry, while the rows are a tree.
fn selected(s: &SessionState) -> Option<(usize, usize)> {
    s.tree_shown()?;
    let row = s.selected_index()?;
    Some((row, s.list.rows[row]))
}

/// Into the tree: on a fold row shows the entries folded in it, on a collapsed directory
/// expands it, and on an expanded one moves to its first entry. False when the rows are not a
/// tree, so the key opens the directory as in the list.
pub fn right(s: &mut SessionState) -> bool {
    if s.tree_shown().is_none() {
        return false;
    }
    let Some((row, i)) = selected(s) else {
        return true;
    };
    let e = &s.snap.entries[i];
    if e.fold > 0 {
        let node = parent(&s.snap.entries, i).map(|p| s.snap.entries[p].node);
        if let (Some(node), Some(tree)) = (node, s.tree.as_mut()) {
            tree.unfold(node);
        }
        return true;
    }
    if !e.is_dir || e.pseudo {
        return true;
    }
    if s.tree.as_ref().is_some_and(|t| t.is_expanded(e.node)) {
        let first = s.list.rows.get(row + 1).copied();
        if first.is_some_and(|c| parent(&s.snap.entries, c) == Some(i)) {
            s.move_to(row + 1);
        }
    } else {
        let target = target_of(&s.snap, i);
        if let Some(tree) = s.tree.as_mut() {
            tree.expand(target);
        }
    }
    true
}

/// Out of the tree: collapses an expanded directory, or moves to the row of the directory the
/// selected one is in. False at the top level and when the rows are not a tree, so the key goes
/// up a directory as in the list.
pub fn left(s: &mut SessionState) -> bool {
    let Some((row, i)) = selected(s) else {
        return false;
    };
    let e = &s.snap.entries[i];
    if e.is_dir && !e.pseudo && s.tree.as_ref().is_some_and(|t| t.is_expanded(e.node)) {
        let node = e.node;
        if let Some(tree) = s.tree.as_mut() {
            tree.collapse(node);
        }
        return true;
    }
    let Some(p) = parent(&s.snap.entries, i) else {
        return false;
    };
    // The parent row is above.
    if let Some(r) = s.list.rows[..row].iter().rposition(|&x| x == p) {
        s.move_to(r);
    }
    true
}

/// Expands the selected directory and, with the next read, its chain of largest entries. Turns
/// tree mode on first when it is off.
pub fn chain(s: &mut SessionState) {
    let i = (s.selected_index())
        .map(|row| s.list.rows[row])
        .filter(|&i| s.snap.entries[i].is_dir && !s.snap.entries[i].pseudo);
    let Some(i) = i else {
        s.status = Some("* expands a directory's largest entries: select a directory".into());
        return;
    };
    if s.tree.is_none() {
        toggle(s);
    }
    let target = target_of(&s.snap, i);
    if let Some(tree) = s.tree.as_mut() {
        tree.expand_chain(target);
    }
}

/// The next fold threshold, 1% → 0.1% → none.
pub fn cycle_fold(s: &mut SessionState) {
    let Some(tree) = s.tree.as_mut() else {
        s.status = Some("z changes what folds in tree mode: T turns it on".into());
        return;
    };
    let ratio = tree.cycle_fold();
    s.status = Some(fold_label(ratio).unwrap_or("nothing folds").into());
}

/// A click on the expander of row `row`: expands or collapses its directory, and selects it.
pub fn toggle_row(s: &mut SessionState, row: usize) {
    let Some(&i) = s.list.rows.get(row) else {
        return;
    };
    s.move_to(row);
    let e = &s.snap.entries[i];
    if !e.is_dir || e.pseudo {
        return;
    }
    let node = e.node;
    let target = target_of(&s.snap, i);
    if let Some(tree) = s.tree.as_mut() {
        if tree.is_expanded(node) {
            tree.collapse(node);
        } else {
            tree.expand(target);
        }
    }
}

/// Before the treemap, whose tiles are the rows of one directory: opens the directory the
/// selected row is in, with the row selected, when that is below the top of the tree.
pub fn treemap_dir(s: &mut SessionState) -> Option<Moved> {
    let (_, i) = selected(s)?;
    parent(&s.snap.entries, i)?;
    let path = path_of(&s.snap, i);
    let root = s.snap.root_path.clone();
    s.go_to(&path, &root)
}

#[cfg(test)]
mod tests;
