use dfdu_tree::{Kind, NodeRef};

use super::super::Estimate;
use super::super::test_util::{Listing, Rig, chunk, chunk_of, close};
use crate::test_util::meta;

/// The estimate, numbered 7, of deleting the nodes at `paths`.
fn estimate(r: &Rig, paths: &[&str]) -> Estimate {
    let items: Vec<NodeRef> = paths.iter().map(|p| r.m.tree.node_ref(r.at(p))).collect();
    r.a.estimate(&r.m, &items, 7)
}

#[test]
fn items_inside_other_items_are_counted_once() {
    let mut r = Rig::scan();
    let root = Listing {
        files: &[("f", 100)],
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    let a = Listing {
        files: &[("g", 50)],
        ..Listing::default()
    };
    r.apply(chunk(1, 0, 2, &a));
    let e = estimate(&r, &["a", "a/g", "f"]);
    assert_eq!((e.id, e.items, e.bytes), (7, 2, 150));
    assert!(e.scanning, "a is still being listed");
    r.apply(close(1, 1));
    r.apply(close(0, 1));
    r.settle();
    let e = estimate(&r, &["a/g", "a"]);
    assert_eq!((e.items, e.bytes, e.linked_out), (1, 50, 0));
    assert!(!e.scanning);
}

#[test]
fn hard_links_kept_outside_the_items_free_nothing() {
    let mut r = Rig::scan();
    let root = Listing {
        subdirs: &["a", "b"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    // One inode with two links: a/x, charged, and b/y.
    let link = |_, f: &(&str, u64)| {
        let mut m = meta(Kind::File, f.1, 77);
        m.nlink = 2;
        m
    };
    let x = Listing {
        files: &[("x", 100)],
        ..Listing::default()
    };
    r.apply(chunk_of(1, 0, 3, &x, link));
    let y = Listing {
        files: &[("y", 100)],
        ..Listing::default()
    };
    r.apply(chunk_of(2, 0, 3, &y, link));
    for dir in [1, 2, 0] {
        r.apply(close(dir, 1));
    }
    r.settle();
    let a = estimate(&r, &["a"]);
    assert_eq!((a.bytes, a.linked_out), (0, 100));
    let both = estimate(&r, &["a", "b"]);
    assert_eq!((both.bytes, both.linked_out), (100, 0));
}
