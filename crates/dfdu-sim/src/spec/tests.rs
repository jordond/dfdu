use super::*;

#[test]
fn a_seed_gives_the_same_draws() {
    let draws = |seed| {
        let mut r = Rng::new(seed);
        (0..50)
            .map(|i| (r.below(i), r.index(7), r.chance(500)))
            .collect::<Vec<_>>()
    };
    assert_eq!(draws(7), draws(7));
    assert_ne!(draws(7), draws(8));
    let mut r = Rng::new(1);
    assert_eq!((r.below(0), r.index(0)), (0, 0));
    for _ in 0..1000 {
        assert!(r.below(3) < 3 && r.index(5) < 5);
    }
    assert!(!(0..1000).any(|_| r.chance(0)));
    assert!((0..1000).all(|_| r.chance(1000)));
}

/// Files get as many links as there are names for their inode, every node knows its parent,
/// and absent entries start out removed.
#[test]
fn the_builder_counts_links_and_records_parents() {
    let mut b = SpecBuilder::new();
    let d = b.dir(ROOT, "d");
    let f = b.file(d, "f", 5000);
    let g = b.link(ROOT, "g", f);
    let h = b.link(d, "h", f);
    let single = b.file(ROOT, "single", 1);
    let gone = b.file(d, "gone", 1);
    b.absent(gone);
    let spec = b.build();
    for n in [f, g, h] {
        let m = spec.nodes[n as usize].meta;
        assert_eq!(
            (m.ino, m.nlink, m.apparent, m.alloc),
            (spec.nodes[f as usize].meta.ino, 3, 5000, 8192)
        );
    }
    assert_eq!(spec.nodes[single as usize].meta.nlink, 1);
    assert_eq!(spec.nodes[d as usize].meta.nlink, 2);
    assert_eq!(
        [d, f, g, h, single].map(|n| spec.parent[n as usize]),
        [ROOT, d, ROOT, d, ROOT]
    );
    assert_eq!(&spec.nodes[ROOT as usize].name[..], ROOT_NAME.as_bytes());
    assert!(spec.is_deleted(gone) && !spec.is_deleted(f));
    assert_eq!(spec.live_children(d).collect::<Vec<_>>(), [f, h]);
    spec.restore(gone);
    assert_eq!(spec.live_children(d).collect::<Vec<_>>(), [f, h, gone]);
}

/// Deleting a directory removes everything below it; an entry of a faulted listing, the root
/// and a removed entry are not for the deleter.
#[test]
fn deleting_takes_the_subtree_and_spares_faulted_listings() {
    let mut b = SpecBuilder::new();
    let d = b.dir(ROOT, "d");
    let inner = b.dir(d, "inner");
    let f = b.file(inner, "f", 1);
    let faulted = b.dir(ROOT, "faulted");
    let in_faulted = b.file(faulted, "x", 1);
    b.fault(faulted, Fault::Eacces);
    let spec = b.build();
    assert!(spec.deletable(d) && spec.deletable(f) && spec.deletable(faulted));
    assert!(!spec.deletable(ROOT));
    assert!(!spec.deletable(in_faulted));
    spec.delete_subtree(d);
    assert!([d, inner, f].iter().all(|&n| spec.is_deleted(n)));
    assert!(!spec.is_deleted(faulted));
    assert!(!spec.deletable(f));
    assert_eq!(spec.live_children(ROOT).collect::<Vec<_>>(), [faulted]);
}
