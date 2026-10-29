//! What the keys do in tree mode: `T` turns it on and off, `→` expands, `←` collapses, `*`
//! expands the chain of largest entries and `z` changes the fold threshold. A click on `▸` or
//! `▾` expands or collapses its directory. The treemap shows the directory of the selected row.

use super::{TreeState, parent, path_of, target_of};
use crate::app::App;

/// `T`: turns tree mode on or off. Turning it off forgets what was expanded.
pub fn toggle(app: &mut App) {
    app.tree = match app.tree {
        Some(_) => None,
        None => Some(TreeState::default()),
    };
    app.request_sort();
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
fn selected(app: &App) -> Option<(usize, usize)> {
    app.tree_shown()?;
    let row = app.selected_index()?;
    Some((row, app.rows[row]))
}

/// `→` in tree mode: on a fold row shows the entries folded in it, on a collapsed directory
/// expands it, and on an expanded one moves to its first entry. False when the rows are not a
/// tree, so the key opens the directory as in the list.
pub fn right(app: &mut App) -> bool {
    if app.tree_shown().is_none() {
        return false;
    }
    let Some((row, i)) = selected(app) else {
        return true;
    };
    let e = &app.snap.entries[i];
    if e.fold > 0 {
        let node = parent(&app.snap.entries, i).map(|p| app.snap.entries[p].node);
        if let (Some(node), Some(tree)) = (node, app.tree.as_mut()) {
            tree.unfold(node);
        }
        return true;
    }
    if !e.is_dir || e.pseudo {
        return true;
    }
    if app.tree.as_ref().is_some_and(|t| t.is_expanded(e.node)) {
        let first = app.rows.get(row + 1).copied();
        if first.is_some_and(|c| parent(&app.snap.entries, c) == Some(i)) {
            app.move_to(row + 1);
        }
    } else {
        let target = target_of(&app.snap, i);
        if let Some(tree) = app.tree.as_mut() {
            tree.expand(target);
        }
    }
    true
}

/// `←` in tree mode: collapses an expanded directory, or moves to the row of the directory
/// the selected one is in. False at the top level and when the rows are not a tree, so the key
/// goes up a directory as in the list.
pub fn left(app: &mut App) -> bool {
    let Some((row, i)) = selected(app) else {
        return false;
    };
    let e = &app.snap.entries[i];
    if e.is_dir && !e.pseudo && app.tree.as_ref().is_some_and(|t| t.is_expanded(e.node)) {
        let node = e.node;
        if let Some(tree) = app.tree.as_mut() {
            tree.collapse(node);
        }
        return true;
    }
    let Some(p) = parent(&app.snap.entries, i) else {
        return false;
    };
    // The parent row is above.
    if let Some(r) = app.rows[..row].iter().rposition(|&x| x == p) {
        app.move_to(r);
    }
    true
}

/// `*`: expands the selected directory and, with the next read, its chain of largest entries.
/// Turns tree mode on first when it is off.
pub fn chain(app: &mut App) {
    let i = app
        .selected_index()
        .map(|row| app.rows[row])
        .filter(|&i| app.snap.entries[i].is_dir && !app.snap.entries[i].pseudo);
    let Some(i) = i else {
        app.status = Some("* expands a directory's largest entries: select a directory".into());
        return;
    };
    if app.tree.is_none() {
        toggle(app);
    }
    let target = target_of(&app.snap, i);
    if let Some(tree) = app.tree.as_mut() {
        tree.expand_chain(target);
    }
}

/// `z`: the next fold threshold, 1% → 0.1% → none.
pub fn cycle_fold(app: &mut App) {
    let Some(tree) = app.tree.as_mut() else {
        app.status = Some("z changes what folds in tree mode: T turns it on".into());
        return;
    };
    let ratio = tree.cycle_fold();
    app.status = Some(fold_label(ratio).unwrap_or("nothing folds").into());
}

/// A click on the `▸` or `▾` of row `row`: expands or collapses its directory, and selects it.
pub fn toggle_row(app: &mut App, row: usize) {
    let Some(&i) = app.rows.get(row) else {
        return;
    };
    app.move_to(row);
    let e = &app.snap.entries[i];
    if !e.is_dir || e.pseudo {
        return;
    }
    let node = e.node;
    let target = target_of(&app.snap, i);
    if let Some(tree) = app.tree.as_mut() {
        if tree.is_expanded(node) {
            tree.collapse(node);
        } else {
            tree.expand(target);
        }
    }
}

/// Before the treemap (`2`), whose tiles are the rows of one directory: opens the directory
/// the selected row is in, with the row selected, when that is below the top of the tree.
pub fn treemap_dir(app: &mut App) {
    let Some((_, i)) = selected(app) else {
        return;
    };
    if parent(&app.snap.entries, i).is_none() {
        return;
    }
    let path = path_of(&app.snap, i);
    let root = app.snap.root_path.clone();
    app.go_to(&path, &root);
}
