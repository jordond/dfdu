use super::super::test_util::{add_dir, add_file, dir_meta};
use super::*;

/// A measured file counts its unshared bytes instead of its allocation, and references what
/// it allocates.
#[test]
fn a_measured_file_counts_its_unshared_bytes() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    let f = add_file(&mut t, d, "f", 8192);
    assert!(
        t.extents.is_empty(),
        "no side table before a node needs one"
    );
    assert_eq!(t.excl(f), 0);
    t.set_measured(f, 1000, true);
    assert!(t.flags(f).has(NodeFlags::MEASURED) && t.flags(f).has(NodeFlags::COMPRESSED));
    assert_eq!(t.excl(f), 1000);
    assert_eq!(t.own(f).alloc, 1000);
    assert_eq!(t.own(f).referenced, 8192);
    assert_eq!(t.total(root).alloc, 4096 * 2 + 1000);
    t.check(&[]).unwrap();
}

/// Shared bytes counted at a directory move its and its ancestors' totals, and the part only
/// referenced from outside the scan goes to `shared_out` too.
#[test]
fn shared_bytes_add_and_subtract_through_the_ancestors() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = add_dir(&mut t, root, "d");
    add_file(&mut t, d, "f", 512);
    let before = t.total(root);
    t.add_shared(d, 300, false);
    t.add_shared(d, 200, true);
    assert_eq!(t.shared_here(d), 500);
    assert_eq!(t.total(root).alloc, before.alloc + 500);
    assert_eq!(t.total(root).shared_out, 200);
    assert_eq!(t.own(d).shared_out, 200);
    t.check(&[]).unwrap();
    t.sub_shared(d, 200, true);
    t.sub_shared(d, 300, false);
    assert_eq!(t.shared_here(d), 0);
    assert_eq!(t.total(root), before);
    t.check(&[]).unwrap();
}
