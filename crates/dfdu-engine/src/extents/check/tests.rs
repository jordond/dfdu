use super::*;
use crate::extents::tests::{m, tree};

/// x and y share [0, 60), homed at a; returns the table, the tree, r and a.
fn shared() -> (ExtentTable, Tree, Ix, Ix) {
    let (mut t, r, [x, y, _]) = tree();
    let mut e = ExtentTable::default();
    for (ix, ino) in [(x, 10), (y, 11)] {
        let n = t.node_ref(ix);
        e.measure(&mut t, (1, ino), n, m(40, &[(0, 60)]));
    }
    let a = t.parent(x).unwrap();
    (e, t, r, a)
}

/// The view of an inode names where it is counted and the segments it references.
#[test]
fn an_inode_is_described_with_its_segments() {
    let (e, t, ..) = shared();
    let d = e.describe(&t, (1, 10));
    assert!(d.starts_with("(1, 10) at /r/a/x excl 40"), "{d}");
    assert!(d.contains("seg 0..60 refs [(1, 10), (1, 11)]"), "{d}");
    assert_eq!(e.describe(&t, (1, 99)), "(1, 99): not in the table");
}

/// A measured inode counted in a rescan's shadow, or at a node detached, is not settled.
#[test]
fn an_inode_counted_out_of_sight_is_not_settled() {
    use crate::test_util::meta;
    use dfdu_tree::Kind;
    let (mut e, mut t, _, a) = shared();
    assert_eq!(e.check_settled(&t), Ok(()));
    let shadow = t.new_root(b"a", &meta(Kind::Dir, 0, 2), false);
    let real = e.inodes[&(1, 10)].counted_at;
    e.inodes.get_mut(&(1, 10)).unwrap().counted_at = t.node_ref(shadow);
    assert_eq!(
        e.check_settled(&t),
        Err(Mismatch::NotVisible {
            inode: (1, 10),
            gone: false
        })
    );
    e.inodes.get_mut(&(1, 10)).unwrap().counted_at = real;
    t.detach(a);
    let err = e.check_settled(&t).unwrap_err();
    assert!(
        matches!(err, Mismatch::NotVisible { gone: true, .. }),
        "{err}"
    );
    assert!(
        err.to_string()
            .ends_with("is not visible (detached or freed)"),
        "{err}"
    );
}

/// A home that is not the lowest common ancestor of its references, and home bytes the tree
/// and the table disagree on, are found.
#[test]
fn wrong_homes_and_home_bytes_are_found() {
    let (mut e, mut t, r, a) = shared();
    assert_eq!(e.check(&t), Ok(()));
    t.add_shared(a, 5, false);
    assert_eq!(
        e.check(&t),
        Err(Mismatch::Bytes {
            path: "/r/a".into(),
            tree: 65,
            table: 60,
        })
    );
    t.sub_shared(a, 5, false);
    let root = t.node_ref(r);
    for seg in e.segs.values_mut() {
        seg.home = Some(root);
    }
    let err = e.check(&t).unwrap_err();
    let Mismatch::Home {
        start,
        end,
        home,
        lca,
        refs,
    } = &err
    else {
        panic!("{err}");
    };
    assert_eq!((*start, *end, &home[..]), (0, 60, "/r"));
    assert_eq!(lca.as_deref(), Some("/r/a"));
    assert_eq!(
        refs,
        &[
            "(1, 10) at /r/a/x visible true",
            "(1, 11) at /r/a/y visible true"
        ]
    );
    assert!(
        err.to_string().starts_with("segment 0..60: home /r"),
        "{err}"
    );
}
