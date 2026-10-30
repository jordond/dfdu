use dfdu_tree::Kind;

use super::*;
use crate::extents::Measurement;
use crate::extents::tests::{alloc, tree};
use crate::test_util::meta;

/// A pass over a table whose homes are all visible moves nothing, and one over an empty table
/// ends at once.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_pass_leaves_visible_homes_as_they_are() {
    let (mut t, r, [x, y, _]) = tree();
    let mut e = ExtentTable::default();
    assert!(e.rehome_step(&mut t, &mut Rehome::default(), 1));
    let shared = Measurement {
        ranges: vec![(0, 60)],
        points: vec![(7000, 50)],
        ..Default::default()
    };
    for (ix, ino) in [(x, 10), (y, 11)] {
        let n = t.node_ref(ix);
        e.measure(&mut t, (1, ino), n, shared.clone());
    }
    let a = t.parent(x).unwrap();
    let before = (t.shared_here(a), t.shared_here(r), alloc(&t, r));
    assert_eq!(before.0, 110, "the range and the point, at a");
    assert!(e.rehome_step(&mut t, &mut Rehome::default(), usize::MAX));
    assert_eq!((t.shared_here(a), t.shared_here(r), alloc(&t, r)), before);
    e.check(&t).unwrap();
}

/// Subtrees moved whole into a rescan that replaces their common ancestor keep their
/// references: what they share stays homed in the tree that went until a pass over the table,
/// a step at a time, homes it in the one that came.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_pass_homes_again_what_lost_its_home_to_a_graft() {
    let (mut t, r, [x, y, z]) = tree();
    let mut e = ExtentTable::default();
    let shared = |ranges: &[(u64, u64)], points: &[(u64, u64)]| Measurement {
        ranges: ranges.to_vec(),
        points: points.to_vec(),
        ..Default::default()
    };
    for (ix, ino, mm) in [
        (x, 10, shared(&[(0, 100)], &[])),
        (y, 11, shared(&[], &[(9000, 100)])),
        (z, 12, shared(&[(0, 100)], &[(9000, 100)])),
    ] {
        let n = t.node_ref(ix);
        e.measure(&mut t, (1, ino), n, mm);
    }
    assert_eq!(t.shared_here(r), 200);
    let before = alloc(&t, r);
    let (a, b) = (t.parent(x).unwrap(), t.parent(z).unwrap());
    let s = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), false);
    let mut pairs = Vec::new();
    for (k, name, ino) in [(a, b"a", 2), (b, b"b", 3)] {
        let p = t.add_dir(s, name, &meta(Kind::Dir, 0, ino));
        let own = t.own(p);
        t.propagate_add(s, &own);
        pairs.push((p, k));
    }
    t.graft(&pairs);
    t.replace(r, s);
    assert_eq!(t.shared_here(s), 0, "its home went with the root replaced");
    let mut pass = Rehome::default();
    let mut steps = 1;
    while !e.rehome_step(&mut t, &mut pass, 1) {
        steps += 1;
    }
    assert!(steps >= 2, "{steps} steps");
    assert_eq!(t.shared_here(s), 200);
    assert_eq!(alloc(&t, s), before);
    e.check(&t).unwrap();
    t.check(&[]).unwrap();
}
