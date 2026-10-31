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

/// `set_flag` changes the flags that say what a node is, totals included.
#[test]
fn set_flag_restates_counted_flags() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, root, "a");
    t.set_flag(a, NodeFlags::MOUNT_ROOT, true);
    t.set_flag(a, NodeFlags::RO_SUBVOL, true);
    assert_eq!((t.total(root).mounts, t.total(root).ro_subvols), (1, 1));
    t.set_flag(a, NodeFlags::MOUNT_ROOT, false);
    assert_eq!(t.total(root).mounts, 0);
    t.check(&[]).unwrap();
}

/// A flag with its own setter, such as `CHARGED` (which only a hard link may have), is refused.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "only by its own setter")]
fn set_flag_refuses_flags_with_their_own_setter() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let f = add_file(&mut t, root, "f", 100);
    t.set_flag(f, NodeFlags::CHARGED, false);
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
