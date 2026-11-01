use std::time::Instant;

use dfdu_tree::HostPath;

use super::*;
use crate::snapshot::{Snapshot, test_entry};
use crate::test_support::sample_with_files;

/// A browser in tree mode of [`sample_with_files`]: `a` (with `x`, `y` holding `p` and `q`,
/// and `z`) and `b` (with `only` holding `deep`), every row read, the leaves files.
fn browser() -> SessionState {
    let t0 = Instant::now();
    let mut s = SessionState::new(t0);
    s.tree = Some(TreeState::default());
    let snap = Snapshot {
        dir_path: HostPath::from("/r"),
        root_path: HostPath::from("/r"),
        dir_node: Some(test_entry(99, "/r", 0).node),
        at_root: true,
        entries: sample_with_files(),
        version: 1,
        ..Default::default()
    };
    let _ = s.refresh(snap, t0, false);
    let _ = s.reorder(t0, false);
    s
}

fn names(s: &SessionState) -> Vec<String> {
    (s.list.rows.iter())
        .map(|&i| String::from_utf8_lossy(&s.snap.entries[i].name).into_owned())
        .collect()
}

#[track_caller]
fn select(s: &mut SessionState, name: &str) {
    let row = names(s).iter().position(|n| n == name);
    s.move_to(row.unwrap_or_else(|| panic!("{name} is not a row")));
}

fn expanded(s: &SessionState, name: &str) -> bool {
    let e = (s.snap.entries.iter()).find(|e| e.name[..] == *name.as_bytes());
    let node = e.expect("an entry").node;
    s.tree.as_ref().is_some_and(|t| t.is_expanded(node))
}

fn selected_name(s: &SessionState) -> String {
    let e = s.selected_entry().expect("a selection");
    String::from_utf8_lossy(&e.name).into_owned()
}

#[test]
fn right_expands_then_goes_in_and_left_comes_back_out() {
    let mut s = browser();
    assert_eq!(names(&s)[..3], ["a", "x", "y"]);
    select(&mut s, "y");
    assert!(right(&mut s));
    assert!(expanded(&s, "y"));
    // Expanded already: into its first entry.
    assert!(right(&mut s));
    assert_eq!(selected_name(&s), "p");
    // On a file, nothing more.
    assert!(right(&mut s));
    assert_eq!(selected_name(&s), "p");
    assert!(left(&mut s));
    assert_eq!(selected_name(&s), "y", "the row of the directory it is in");
    assert!(left(&mut s));
    assert!(!expanded(&s, "y"), "collapsed");
    select(&mut s, "a");
    assert!(!left(&mut s), "at the top the key goes up a directory");
}

#[test]
fn the_keys_leave_a_list_that_is_no_tree_alone() {
    let mut s = browser();
    toggle(&mut s);
    assert!(s.tree.is_none());
    assert!(!right(&mut s) && !left(&mut s));
    cycle_fold(&mut s);
    assert_eq!(
        s.status.as_deref(),
        Some("z changes what folds in tree mode: T turns it on")
    );
    toggle(&mut s);
    assert!(s.tree.is_some());
}

#[test]
fn z_steps_through_the_fold_thresholds() {
    let mut s = browser();
    let mut said = Vec::new();
    for _ in 0..3 {
        cycle_fold(&mut s);
        said.push(s.status.clone().unwrap_or_default());
    }
    assert_eq!(said, ["fold < 0.1%", "nothing folds", "fold < 1%"]);
    assert_eq!(fold_label(0.0), None);
}

#[test]
fn star_expands_a_directory_and_turns_tree_mode_on() {
    let mut s = browser();
    select(&mut s, "p");
    chain(&mut s);
    assert!(
        (s.status.as_deref()).is_some_and(|m| m.starts_with("* expands")),
        "a file"
    );
    toggle(&mut s);
    s.move_to(6);
    chain(&mut s);
    let tree = s.tree.as_ref().expect("tree mode is on");
    let target = tree.chain.as_ref().expect("a chain to expand");
    assert_eq!(target.path, HostPath::from("/r").join(b"b"));
}

#[test]
fn a_click_on_the_expander_expands_and_collapses() {
    let mut s = browser();
    toggle_row(&mut s, 6);
    assert_eq!(selected_name(&s), "b");
    assert!(expanded(&s, "b"));
    toggle_row(&mut s, 6);
    assert!(!expanded(&s, "b"));
    // A file's row is only selected.
    toggle_row(&mut s, 1);
    assert_eq!(selected_name(&s), "x");
    assert!(!expanded(&s, "x"));
}

#[test]
fn the_map_opens_the_directory_of_a_row_below_the_top() {
    let mut s = browser();
    select(&mut s, "p");
    let moved = treemap_dir(&mut s).expect("a move");
    let root = HostPath::from("/r");
    assert_eq!(s.target.path, root.join(b"a").join(b"y"));
    assert_eq!(moved.opened, Some(root.join(b"a").join(b"y")));
    assert_eq!(
        s.selected.as_ref().map(|sel| &sel.name[..]),
        Some(&b"p"[..])
    );
    // At the top the directory shown already is the one.
    let mut s = browser();
    assert!(treemap_dir(&mut s).is_none());
    assert_eq!(s.target.path, HostPath::default());
}
