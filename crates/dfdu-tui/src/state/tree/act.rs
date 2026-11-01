//! What the keys do in tree mode: `T` turns it on and off, `→` expands, `←` collapses, `*`
//! expands the chain of largest entries and `z` changes the fold threshold. A click on `▸` or
//! `▾` expands or collapses its directory. The treemap shows the directory of the selected row.

use super::{TreeState, parent, path_of, target_of};
use crate::app::App;

/// `T`: turns tree mode on or off. Turning it off forgets what was expanded.
pub fn toggle(app: &mut App) {
    app.session.tree = match app.session.tree {
        Some(_) => None,
        None => Some(TreeState::default()),
    };
    app.session.request_sort();
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
    let row = app.session.selected_index()?;
    Some((row, app.session.list.rows[row]))
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
    let e = &app.session.snap.entries[i];
    if e.fold > 0 {
        let node = parent(&app.session.snap.entries, i).map(|p| app.session.snap.entries[p].node);
        if let (Some(node), Some(tree)) = (node, app.session.tree.as_mut()) {
            tree.unfold(node);
        }
        return true;
    }
    if !e.is_dir || e.pseudo {
        return true;
    }
    if app
        .session
        .tree
        .as_ref()
        .is_some_and(|t| t.is_expanded(e.node))
    {
        let first = app.session.list.rows.get(row + 1).copied();
        if first.is_some_and(|c| parent(&app.session.snap.entries, c) == Some(i)) {
            app.session.move_to(row + 1);
        }
    } else {
        let target = target_of(&app.session.snap, i);
        if let Some(tree) = app.session.tree.as_mut() {
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
    let e = &app.session.snap.entries[i];
    if e.is_dir
        && !e.pseudo
        && app
            .session
            .tree
            .as_ref()
            .is_some_and(|t| t.is_expanded(e.node))
    {
        let node = e.node;
        if let Some(tree) = app.session.tree.as_mut() {
            tree.collapse(node);
        }
        return true;
    }
    let Some(p) = parent(&app.session.snap.entries, i) else {
        return false;
    };
    // The parent row is above.
    if let Some(r) = app.session.list.rows[..row].iter().rposition(|&x| x == p) {
        app.session.move_to(r);
    }
    true
}

/// `*`: expands the selected directory and, with the next read, its chain of largest entries.
/// Turns tree mode on first when it is off.
pub fn chain(app: &mut App) {
    let i = app
        .session
        .selected_index()
        .map(|row| app.session.list.rows[row])
        .filter(|&i| app.session.snap.entries[i].is_dir && !app.session.snap.entries[i].pseudo);
    let Some(i) = i else {
        app.session.status =
            Some("* expands a directory's largest entries: select a directory".into());
        return;
    };
    if app.session.tree.is_none() {
        toggle(app);
    }
    let target = target_of(&app.session.snap, i);
    if let Some(tree) = app.session.tree.as_mut() {
        tree.expand_chain(target);
    }
}

/// `z`: the next fold threshold, 1% → 0.1% → none.
pub fn cycle_fold(app: &mut App) {
    let Some(tree) = app.session.tree.as_mut() else {
        app.session.status = Some("z changes what folds in tree mode: T turns it on".into());
        return;
    };
    let ratio = tree.cycle_fold();
    app.session.status = Some(fold_label(ratio).unwrap_or("nothing folds").into());
}

/// A click on the `▸` or `▾` of row `row`: expands or collapses its directory, and selects it.
pub fn toggle_row(app: &mut App, row: usize) {
    let Some(&i) = app.session.list.rows.get(row) else {
        return;
    };
    app.session.move_to(row);
    let e = &app.session.snap.entries[i];
    if !e.is_dir || e.pseudo {
        return;
    }
    let node = e.node;
    let target = target_of(&app.session.snap, i);
    if let Some(tree) = app.session.tree.as_mut() {
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
    if parent(&app.session.snap.entries, i).is_none() {
        return;
    }
    let path = path_of(&app.session.snap, i);
    let root = app.session.snap.root_path.clone();
    app.go_to(&path, &root);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_tree::HostPath;

    use super::super::tests::sample_with_files;
    use super::*;
    use crate::tests::{row_names, select, snap_with_totals};
    use crate::theme::Theme;

    /// A browser in tree mode of [`sample`]: `a` (with `x`, `y` holding `p` and `q`, and `z`)
    /// and `b` (with `only` holding `deep`), every row read, the leaves files.
    fn browser() -> App {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.session.tree = Some(TreeState::default());
        let mut snap = snap_with_totals(sample_with_files(), 1);
        snap.root_path = HostPath::from("/r");
        app.refresh(snap, t0);
        app.reorder(t0);
        app
    }

    fn expanded(app: &App, name: &str) -> bool {
        let e = app
            .session
            .snap
            .entries
            .iter()
            .find(|e| e.name[..] == *name.as_bytes());
        let node = e.expect("an entry").node;
        app.session
            .tree
            .as_ref()
            .is_some_and(|t| t.is_expanded(node))
    }

    fn selected_name(app: &App) -> String {
        let e = app.session.selected_entry().expect("a selection");
        String::from_utf8_lossy(&e.name).into_owned()
    }

    #[test]
    fn right_expands_then_goes_in_and_left_comes_back_out() {
        let mut app = browser();
        assert_eq!(row_names(&app)[..3], ["a", "x", "y"]);
        select(&mut app, "y");
        assert!(right(&mut app));
        assert!(expanded(&app, "y"));
        // Expanded already: into its first entry.
        assert!(right(&mut app));
        assert_eq!(selected_name(&app), "p");
        // On a file, nothing more.
        assert!(right(&mut app));
        assert_eq!(selected_name(&app), "p");
        assert!(left(&mut app));
        assert_eq!(
            selected_name(&app),
            "y",
            "the row of the directory it is in"
        );
        assert!(left(&mut app));
        assert!(!expanded(&app, "y"), "collapsed");
        select(&mut app, "a");
        assert!(!left(&mut app), "at the top the key goes up a directory");
    }

    #[test]
    fn the_keys_leave_a_list_that_is_no_tree_alone() {
        let mut app = browser();
        toggle(&mut app);
        assert!(app.session.tree.is_none());
        assert!(!right(&mut app) && !left(&mut app));
        cycle_fold(&mut app);
        assert_eq!(
            app.session.status.as_deref(),
            Some("z changes what folds in tree mode: T turns it on")
        );
        toggle(&mut app);
        assert!(app.session.tree.is_some());
    }

    #[test]
    fn z_steps_through_the_fold_thresholds() {
        let mut app = browser();
        let mut said = Vec::new();
        for _ in 0..3 {
            cycle_fold(&mut app);
            said.push(app.session.status.clone().unwrap_or_default());
        }
        assert_eq!(said, ["fold < 0.1%", "nothing folds", "fold < 1%"]);
        assert_eq!(fold_label(0.0), None);
    }

    #[test]
    fn star_expands_a_directory_and_turns_tree_mode_on() {
        let mut app = browser();
        select(&mut app, "p");
        chain(&mut app);
        assert!(
            app.session
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("* expands"))
        );
        toggle(&mut app);
        app.session.move_to(6);
        chain(&mut app);
        let tree = app.session.tree.as_ref().expect("tree mode is on");
        let target = tree.chain.as_ref().expect("a chain to expand");
        assert_eq!(target.path, HostPath::from("/r").join(b"b"));
    }

    #[test]
    fn a_click_on_the_glyph_expands_and_collapses() {
        let mut app = browser();
        toggle_row(&mut app, 6);
        assert_eq!(selected_name(&app), "b");
        assert!(expanded(&app, "b"));
        toggle_row(&mut app, 6);
        assert!(!expanded(&app, "b"));
        // A file's row is only selected.
        toggle_row(&mut app, 1);
        assert_eq!(selected_name(&app), "x");
        assert!(!expanded(&app, "x"));
    }

    #[test]
    fn the_map_opens_the_directory_of_a_row_below_the_top() {
        let mut app = browser();
        select(&mut app, "p");
        treemap_dir(&mut app);
        let root = HostPath::from("/r");
        assert_eq!(app.session.target.path, root.join(b"a").join(b"y"));
        assert_eq!(
            app.session.selected.as_ref().map(|s| &s.name[..]),
            Some(&b"p"[..])
        );
        // At the top the directory shown already is the one.
        let mut app = browser();
        treemap_dir(&mut app);
        assert_eq!(app.session.target.path, HostPath::default());
    }
}
