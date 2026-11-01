use super::*;
use crate::snapshot::test_entry;
use crate::test_support::sample;

#[test]
fn parent_follows_the_rows_up() {
    let es = sample();
    assert_eq!(parent(&es, 3), Some(2));
    assert_eq!(parent(&es, 2), Some(0));
    assert_eq!(parent(&es, 8), Some(7));
    assert_eq!(parent(&es, 0), None, "a top row has no parent");
    assert_eq!(parent(&es, 6), None);
}

#[test]
fn paths_are_the_names_down_from_the_directory() {
    let snap = Snapshot {
        dir_path: HostPath::from("/r"),
        entries: sample(),
        ..Default::default()
    };
    assert_eq!(
        path_of(&snap, 3),
        HostPath::from("/r").join(b"a").join(b"y").join(b"p")
    );
    assert_eq!(path_of(&snap, 6), HostPath::from("/r").join(b"b"));
    let t = target_of(&snap, 8);
    assert_eq!(
        t.path,
        HostPath::from("/r").join(b"b").join(b"only").join(b"deep")
    );
    assert_eq!(t.node, Some(snap.entries[8].node));
}

fn target(e: &EntryData, path: &str) -> Target {
    Target {
        node: Some(e.node),
        path: HostPath::from(path),
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
