use std::path::PathBuf;

use super::*;
use crate::model::snapshot::{EntryData, test_entry};
use crate::state::order::{SortSpec, tree_order};

/// Row `i` named `name` of `size` bytes, below row `parent`.
fn row(i: u32, name: &str, size: u64, parent: Option<usize>, entries: &[EntryData]) -> EntryData {
    let (depth, parent) = match parent {
        Some(p) => (entries[p].depth + 1, u32::try_from(p).expect("small")),
        None => (0, u32::MAX),
    };
    EntryData {
        is_dir: true,
        depth,
        parent,
        ..test_entry(i, name, size)
    }
}

/// ```text
/// a
/// ├─x
/// ├─y
/// │ ├─p
/// │ └─q
/// └─z
/// b
/// └─only
///   └─deep
/// ```
fn sample() -> Vec<EntryData> {
    let mut es: Vec<EntryData> = Vec::new();
    for (name, size, parent) in [
        ("a", 100, None),
        ("x", 50, Some(0)),
        ("y", 40, Some(0)),
        ("p", 30, Some(2)),
        ("q", 10, Some(2)),
        ("z", 10, Some(0)),
        ("b", 90, None),
        ("only", 90, Some(6)),
        ("deep", 90, Some(7)),
    ] {
        let i = u32::try_from(es.len()).expect("small");
        let e = row(i, name, size, parent, &es);
        es.push(e);
    }
    es
}

fn guides(es: &[EntryData]) -> Vec<String> {
    let rows = tree_order(es, SortSpec::default());
    assert_eq!(rows, (0..es.len()).collect::<Vec<_>>(), "preorder by size");
    let last = last_children(es, &rows);
    rows.iter().map(|&i| guide(es, &last, i)).collect()
}

#[test]
fn guides_for_nested_last_and_single_children() {
    let g = guides(&sample());
    assert_eq!(
        g,
        [
            "",
            "├─",
            "├─",
            "│ ├─",
            "│ └─",
            "└─", // a: nested, the last child closes it
            "",
            "└─",
            "  └─", // b: single children all the way down
        ]
    );
}

#[test]
fn guides_follow_the_display_order() {
    // z grows past y: y becomes the last of a's children, and z no longer is.
    let mut es = sample();
    es[5].size = 45;
    let rows = tree_order(&es, SortSpec::default());
    let names: Vec<&[u8]> = rows.iter().map(|&i| &es[i].name[..]).collect();
    assert_eq!(names[..4], [&b"a"[..], b"x", b"z", b"y"]);
    let last = last_children(&es, &rows);
    assert_eq!(guide(&es, &last, 5), "├─", "z is no longer last");
    assert_eq!(guide(&es, &last, 2), "└─");
    assert_eq!(
        guide(&es, &last, 3),
        "  ├─",
        "y is last: no line down past it"
    );
}

#[test]
fn inside_follows_the_parents() {
    let es = sample();
    assert!(inside(&es, 3, 2));
    assert!(inside(&es, 3, 0));
    assert!(!inside(&es, 3, 6));
    assert!(!inside(&es, 0, 0), "a row is not inside itself");
}

#[test]
fn paths_are_the_names_down_from_the_directory() {
    let snap = Snapshot {
        dir_path: PathBuf::from("/r"),
        entries: sample(),
        ..Default::default()
    };
    assert_eq!(path_of(&snap, 3), PathBuf::from("/r/a/y/p"));
    assert_eq!(path_of(&snap, 6), PathBuf::from("/r/b"));
    let t = target_of(&snap, 8);
    assert_eq!(t.path, PathBuf::from("/r/b/only/deep"));
    assert_eq!(t.node, Some(snap.entries[8].node));
}

fn target(e: &EntryData, path: &str) -> Target {
    Target {
        node: Some(e.node),
        path: PathBuf::from(path),
        ident: None,
    }
}

#[test]
fn collapsing_forgets_what_was_expanded_inside() {
    let es = sample();
    let mut t = TreeState::default();
    t.expand(target(&es[0], "/r/a"));
    t.expand(target(&es[2], "/r/a/y"));
    t.expand(target(&es[6], "/r/b"));
    t.expand(target(&es[1], "/r/ab"));
    let g = t.generation;
    t.collapse(es[0].node);
    assert!(!t.is_expanded(es[0].node) && !t.is_expanded(es[2].node));
    assert!(t.is_expanded(es[6].node));
    assert!(
        t.is_expanded(es[1].node),
        "a sibling sharing a prefix of the name stays"
    );
    assert!(t.generation > g, "a new read is due");
}

#[test]
fn fold_threshold_cycles_and_lowers_for_one_parent() {
    let es = sample();
    let mut t = TreeState::default();
    assert!((t.fold_ratio - 0.01).abs() < f64::EPSILON);
    assert!((t.cycle_fold() - 0.001).abs() < f64::EPSILON);
    assert!(t.cycle_fold().abs() < f64::EPSILON, "off");
    assert!((t.cycle_fold() - 0.01).abs() < f64::EPSILON);

    t.expand(target(&es[0], "/r/a"));
    t.expand(target(&es[6], "/r/b"));
    t.unfold(es[0].node);
    assert!(t.fold_ratio_of(es[0].node).abs() < f64::EPSILON);
    assert!((t.fold_ratio_of(es[6].node) - 0.01).abs() < f64::EPSILON);
}

#[test]
fn apply_follows_a_rescan_swap_and_drops_what_is_gone() {
    let es = sample();
    let mut t = TreeState::default();
    t.expand(target(&es[0], "/r/a"));
    t.expand(target(&es[6], "/r/b"));
    t.unfold(es[0].node);
    let new = test_entry(50, "a", 100).node;
    t.apply(&Found {
        generation: t.generation,
        moved: vec![(es[0].node, new)],
        gone: vec![es[6].node],
        ..Found::default()
    });
    assert!(t.is_expanded(new) && !t.is_expanded(es[0].node));
    assert_eq!(t.expanded[&new].target.node, Some(new));
    assert!(
        t.fold_ratio_of(new).abs() < f64::EPSILON,
        "its threshold moves with it"
    );
    assert!(!t.is_expanded(es[6].node));
}

#[test]
fn stale_when_the_snapshot_was_read_for_another_state() {
    let mut t = TreeState::default();
    let mut snap = Snapshot::default();
    assert!(!stale(None, &snap));
    assert!(stale(Some(&t), &snap), "tree mode turned on");
    snap.tree = Some(Found::default());
    assert!(!stale(Some(&t), &snap));
    t.cycle_fold();
    assert!(stale(Some(&t), &snap));
    assert!(stale(None, &snap), "tree mode turned off");
}
