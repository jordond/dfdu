use super::super::test_util::{add_dir, add_file, dir_meta, file_meta};
use super::*;

#[test]
fn totals_propagate_and_detach_subtracts() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, root, "a");
    add_file(&mut t, a, "f1", 100);
    add_file(&mut t, a, "f2", 50);
    add_file(&mut t, root, "g", 7);
    assert_eq!(t.total(root).apparent, 10 + 10 + 100 + 50 + 7);
    assert_eq!(t.total(root).items, 5);
    t.check(&[]).unwrap();

    let a_ref = t.node_ref(a);
    t.detach(a);
    assert_eq!(t.total(root).apparent, 17);
    assert!(
        t.resolve(a_ref).is_none(),
        "detached nodes stop resolving immediately"
    );
    t.check(&[]).unwrap();
    while !t.free_step(1, |_| {}) {}
    assert_eq!(t.live_nodes(), 2);
}

#[test]
fn freed_slot_reuse_bumps_generation() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let f = add_file(&mut t, root, "f", 1);
    let old = t.node_ref(f);
    t.detach(f);
    t.free_step(usize::MAX, |_| {});
    let g = add_file(&mut t, root, "g", 2);
    assert_eq!(g.0, f.0, "slot reused");
    assert!(t.resolve(old).is_none());
    assert!(t.resolve(t.node_ref(g)).is_some());
}

#[test]
fn freeing_is_bottom_up() {
    // A deep subtree freed one unit at a time never leaves a live node under a freed slot.
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let top = add_dir(&mut t, root, "top");
    let mut cur = top;
    let mut refs = vec![];
    for i in 0..20 {
        cur = add_dir(&mut t, cur, &format!("d{i}"));
        refs.push(t.node_ref(cur));
        add_file(&mut t, cur, "x", 3);
    }
    t.detach(top);
    loop {
        let done = t.free_step(1, |_| {});
        // Allocate into freed slots between steps, as the applier would.
        if !t.free_nodes.is_empty() {
            add_file(&mut t, root, "new", 1);
            for r in &refs {
                if let Some(ix) = t.resolve(*r) {
                    assert!(!t.is_visible(ix), "a freed subtree node became visible");
                }
            }
        }
        if done {
            break;
        }
    }
    t.check(&[]).unwrap();
}

#[test]
fn replace_swaps_totals() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, root, "a");
    add_file(&mut t, a, "old", 100);
    let shadow = t.new_root(b"a", &dir_meta(), false);
    add_file(&mut t, shadow, "new", 30);
    t.check(&[shadow]).unwrap();
    t.replace(a, shadow);
    assert_eq!(t.total(root).apparent, 10 + 10 + 30);
    assert!(t.is_visible(shadow));
    t.check(&[]).unwrap();
}

#[test]
fn a_graft_moves_the_kept_subtree_into_the_placeholder() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let old = add_dir(&mut t, root, "a");
    let kept = add_dir(&mut t, old, "k");
    add_file(&mut t, kept, "f", 100);
    let shadow = t.new_root(b"a", &dir_meta(), false);
    let placeholder = add_dir(&mut t, shadow, "renamed");
    add_file(&mut t, shadow, "new", 30);
    let kept_ref = t.node_ref(kept);
    t.graft(&[(placeholder, kept)]);
    t.check(&[shadow]).unwrap();
    assert_eq!(t.total(old).apparent, 10);
    assert_eq!(t.total(shadow).apparent, 10 + 10 + 100 + 30);
    assert_eq!(t.name(kept), b"renamed");
    assert_eq!(t.parent(kept), Some(shadow));
    assert_eq!(t.resolve(kept_ref), Some(kept));
    t.replace(old, shadow);
    assert_eq!(t.total(root).apparent, 10 + 10 + 10 + 100 + 30);
    t.check(&[]).unwrap();
    while !t.free_step(1, |_| {}) {}
    assert!(t.resolve_visible(kept_ref).is_some());
    t.check(&[]).unwrap();
}

/// Many kept directories of one parent, in the segment and the blocks of a wide directory, go
/// to their placeholders at once; the others stay in order.
#[test]
fn a_graft_of_many_keeps_both_sides_in_order() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let old = add_dir(&mut t, root, "a");
    let shadow = t.new_root(b"a", &dir_meta(), false);
    let mut pairs = Vec::new();
    let mut stays = Vec::new();
    for i in 0..3_000 {
        let k = add_dir(&mut t, old, &format!("k{i}"));
        add_file(&mut t, k, "f", 1);
        let p = add_dir(&mut t, shadow, &format!("k{i}"));
        if i % 3 == 0 {
            stays.push(k);
        } else {
            pairs.push((p, k));
        }
    }
    let before = (t.total(old).apparent, t.total(shadow).apparent);
    t.graft(&pairs);
    t.check(&[shadow]).unwrap();
    assert_eq!(t.children(old).collect::<Vec<_>>(), stays);
    let moved = 2_000 * (10 + 1);
    assert_eq!(t.total(old).apparent, before.0 - moved);
    assert_eq!(t.total(shadow).apparent, before.1 + 2_000);
    let kids: Vec<Ix> = t.children(shadow).collect();
    for (i, &c) in kids.iter().enumerate() {
        assert_eq!(t.name(c), format!("k{i}").as_bytes());
        assert_eq!(i % 3 != 0, pairs.iter().any(|&(_, k)| k == c));
    }
    t.replace(old, shadow);
    t.check(&[]).unwrap();
    while !t.free_step(64, |_| {}) {}
    t.check(&[]).unwrap();
}

#[test]
fn replacing_a_visible_root_takes_its_place() {
    let mut t = Tree::new();
    let first = t.new_root(b"/a", &dir_meta(), true);
    let root = t.new_root(b"/r", &dir_meta(), true);
    let shadow = t.new_root(b"/r", &dir_meta(), false);
    assert!(!t.is_visible(shadow));
    t.replace(root, shadow);
    assert_eq!(t.roots().collect::<Vec<_>>(), [first, shadow]);
    assert!(t.is_visible(shadow) && !t.is_visible(root));
    t.check(&[]).unwrap();
}

#[test]
fn hardlink_charging() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let meta = file_meta(100, 42, 2);
    let a = t.add_file(root, b"a", &meta, true, None);
    let b = t.add_file(root, b"b", &meta, false, None);
    let delta = {
        let mut sum = t.own(a);
        sum.add(&t.own(b));
        sum
    };
    t.propagate_add(root, &delta);
    assert_eq!(t.total(root).apparent, 10 + 100);
    t.set_charged(a, false);
    t.set_charged(b, true);
    assert_eq!(t.total(root).apparent, 10 + 100);
    t.check(&[]).unwrap();
}

/// An error is counted once per node: in a directory's own total, or a file's parent's.
#[test]
fn set_error_counts_each_node_once() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    let f = add_file(&mut t, d, "f", 1);
    t.set_error(f, ErrKind::PermissionDenied);
    t.set_error(f, ErrKind::NotFound);
    assert_eq!((t.total(d).errors, t.total(root).errors), (1, 1));
    assert_eq!(
        t.error(f),
        Some(ErrKind::NotFound),
        "the later error is kept"
    );
    t.set_error(d, ErrKind::Other);
    assert_eq!((t.total(d).errors, t.total(root).errors), (2, 2));
    t.check(&[]).unwrap();
}

/// Changing a node's own stat moves the difference into every ancestor; a change that leaves
/// the totals alone still bumps the version.
#[test]
fn update_meta_restates_the_ancestors() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    let f = add_file(&mut t, d, "f", 100);
    t.update_meta(f, &file_meta(300, 7, 1));
    assert_eq!(t.total(root).apparent, 10 + 10 + 300);
    assert_eq!(t.total(d).alloc, 4096 + 300);
    let v = t.version();
    t.update_meta(f, &file_meta(300, 8, 1));
    assert!(t.version() > v);
    assert_eq!(t.dev_ino(f), (1, 8));
    t.check(&[]).unwrap();
}

/// Freeing reports hard links and measured files, and nothing else.
#[test]
fn free_step_reports_links_and_measured_files() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    let link = t.add_file(d, b"link", &file_meta(5, 42, 2), true, None);
    let measured = add_file(&mut t, d, "m", 4096);
    t.set_measured(measured, 100, false);
    add_file(&mut t, d, "plain", 9);
    let (link_ref, measured_ref) = (t.node_ref(link), t.node_ref(measured));
    t.detach(d);
    let mut seen = Vec::new();
    assert!(t.free_step(usize::MAX, |l| seen.push(l)));
    seen.sort_by_key(|l| l.ino);
    let got: Vec<_> = seen
        .iter()
        .map(|l| (l.node, l.ino, l.charged, l.hardlink, l.measured))
        .collect();
    assert_eq!(
        got,
        [
            (link_ref, 42, true, true, false),
            (measured_ref, 4096, false, false, true)
        ]
    );
    assert!(t.is_freed(link_ref) && t.is_freed(measured_ref));
    assert_eq!(t.live_nodes(), 1);
}
