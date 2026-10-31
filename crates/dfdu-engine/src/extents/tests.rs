use super::*;
use crate::test_support::meta;
use dfdu_tree::{Kind, Totals};

/// /r/{a/{x,y}, b/{z}} with 100-byte files; returns (tree, root, [x, y, z]).
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
pub(super) fn tree() -> (Tree, Ix, [Ix; 3]) {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
    let b = t.add_dir(r, b"b", &meta(Kind::Dir, 0, 3));
    t.propagate_add(r, &(t.own(a)));
    t.propagate_add(r, &(t.own(b)));
    let f = |t: &mut Tree, d: Ix, n: &[u8], ino| {
        let x = t.add_file(d, n, &meta(Kind::File, 100, ino), false, None);
        let own = t.own(x);
        t.propagate_add(d, &own);
        x
    };
    let x = f(&mut t, a, b"x", 10);
    let y = f(&mut t, a, b"y", 11);
    let z = f(&mut t, b, b"z", 12);
    (t, r, [x, y, z])
}

pub(super) fn m(excl: u64, ranges: &[(u64, u64)]) -> Measurement {
    Measurement {
        excl,
        ranges: ranges.to_vec(),
        ..Default::default()
    }
}

pub(super) fn alloc(t: &Tree, ix: Ix) -> u64 {
    t.total(ix).alloc
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn shared_data_is_counted_once_at_the_common_ancestor() {
    let (mut t, r, [x, y, z]) = tree();
    let mut e = ExtentTable::default();
    // 100-byte files: x is all shared; y shares [0, 60) with it, z shares [40, 100).
    let n = t.node_ref(x);
    e.measure(&mut t, (1, 10), n, m(0, &[(0, 100)]));
    assert_eq!(alloc(&t, x), 100, "alone, x holds its shared bytes");
    let n = t.node_ref(y);
    e.measure(&mut t, (1, 11), n, m(40, &[(0, 60)]));
    let n = t.node_ref(z);
    e.measure(&mut t, (1, 12), n, m(40, &[(40, 100)]));
    let a = t.parent(x).unwrap();
    let b = t.parent(z).unwrap();
    // [0, 40) x+y -> a; [40, 60) x+y+z -> r; [60, 100) x+z -> r.
    assert_eq!(t.shared_here(a), 40);
    assert_eq!(t.shared_here(r), 60);
    assert_eq!((alloc(&t, x), alloc(&t, y), alloc(&t, z)), (0, 40, 40));
    assert_eq!(alloc(&t, a), 80);
    assert_eq!(alloc(&t, b), 40);
    assert_eq!(alloc(&t, r), 180, "every byte once");
    assert_eq!(t.total(r).referenced, 300);
    e.check(&t).unwrap();
    t.check(&[]).unwrap();

    // Deleting b: what z shared moves down to the files still referencing it.
    t.detach(b);
    let mut freed = vec![];
    while !t.free_step(100, |f| {
        assert!(f.measured);
        freed.push((f.dev, f.ino));
    }) {}
    for i in freed {
        e.remove(&mut t, i);
    }
    // [40, 60) x+y -> a; [60, 100) x alone.
    assert_eq!(t.shared_here(a), 60);
    assert_eq!(t.shared_here(r), 0);
    assert_eq!(alloc(&t, x), 40);
    assert_eq!(alloc(&t, r), 140);
    e.check(&t).unwrap();
    t.check(&[]).unwrap();
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn order_does_not_matter() {
    let orders = [[0, 1, 2], [2, 1, 0], [1, 2, 0]];
    let mut results: Vec<Vec<Totals>> = vec![];
    for o in orders {
        let (mut t, r, files) = tree();
        let mut e = ExtentTable::default();
        let ms = [
            m(1, &[(0, 50), (70, 90)]),
            m(2, &[(10, 80)]),
            m(3, &[(20, 30), (85, 120)]),
        ];
        for &i in &o {
            let n = t.node_ref(files[i]);
            e.measure(&mut t, (1, 10 + i as u64), n, ms[i].clone());
        }
        e.check(&t).unwrap();
        t.check(&[]).unwrap();
        let a = t.parent(files[0]).unwrap();
        let b = t.parent(files[2]).unwrap();
        results.push(
            [r, a, b, files[0], files[1], files[2]]
                .map(|n| t.total(n))
                .to_vec(),
        );
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:#?}");
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn compressed_extents_match_by_start() {
    let (mut t, r, [x, _, z]) = tree();
    let mut e = ExtentTable::default();
    let c = |len| Measurement {
        compressed: true,
        points: vec![(5000, len)],
        ..Default::default()
    };
    let n = t.node_ref(x);
    e.measure(&mut t, (1, 10), n, c(128));
    let n = t.node_ref(z);
    e.measure(&mut t, (1, 12), n, c(64));
    assert_eq!(t.shared_here(r), 128);
    // The compressed extent once, plus y, which is not measured.
    assert_eq!(alloc(&t, r), 128 + 100);
    e.remove(&mut t, (1, 10));
    assert_eq!(t.shared_here(r), 0);
    assert_eq!(t.shared_here(z), 64);
    e.check(&t).unwrap();
    t.check(&[]).unwrap();
}
