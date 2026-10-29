use super::*;

fn dir_meta() -> Meta {
    Meta {
        kind: Kind::Dir,
        apparent: 10,
        alloc: 4096,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 2,
        mount: 0,
    }
}
fn file_meta(size: u64, ino: u64, nlink: u32) -> Meta {
    Meta {
        kind: Kind::File,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino,
        nlink,
        mount: 0,
    }
}

fn add_file(t: &mut Tree, p: Ix, name: &str, size: u64) -> Ix {
    let f = t.add_file(p, name.as_bytes(), &file_meta(size, size, 1), false, None);
    let own = t.own(f);
    t.propagate_add(p, &own);
    f
}

fn add_dir(t: &mut Tree, p: Ix, name: &str) -> Ix {
    let d = t.add_dir(p, name.as_bytes(), &dir_meta());
    let own = t.own(d);
    t.propagate_add(p, &own);
    d
}

#[test]
fn cmp_paths_orders_like_path() {
    let mut t = Tree::new();
    let mut nodes = Vec::new();
    for root in ["/", "/r", "/r/", "/r-x"] {
        let r = t.new_root(root.as_bytes(), &dir_meta(), true);
        nodes.push(r);
        for a in ["a", "a-b", "a.b", "b", "ab"] {
            let d = add_dir(&mut t, r, a);
            nodes.push(d);
            for f in ["a", "a-b", "", "z"] {
                nodes.push(add_file(&mut t, d, f, 1));
            }
            let e = add_dir(&mut t, d, "");
            nodes.push(add_file(&mut t, e, "a", 1));
        }
    }
    let (mut ca, mut cb) = (Vec::new(), Vec::new());
    for &a in &nodes {
        t.ancestors(a, &mut ca);
        for &b in &nodes {
            t.ancestors(b, &mut cb);
            assert_eq!(
                t.cmp_paths(&ca, &cb),
                t.path(a).cmp(&t.path(b)),
                "{:?} {:?}",
                String::from_utf8_lossy(&t.path(a)),
                String::from_utf8_lossy(&t.path(b))
            );
        }
    }
}

#[test]
fn leaves_are_every_non_directory_below() {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &dir_meta(), true);
    let a = add_dir(&mut t, r, "a");
    let f1 = add_file(&mut t, r, "f1", 1);
    let f2 = add_file(&mut t, a, "f2", 2);
    let empty = add_dir(&mut t, a, "empty");
    let mut seen = Vec::new();
    t.for_each_leaf(r, |ix| seen.push(ix));
    seen.sort();
    let mut want = vec![f1, f2];
    want.sort();
    assert_eq!(seen, want);
    let mut none = Vec::new();
    t.for_each_leaf(empty, |ix| none.push(ix));
    assert_eq!(none, Vec::<Ix>::new());
    // A leaf is its own only leaf.
    let mut one = Vec::new();
    t.for_each_leaf(f2, |ix| one.push(ix));
    assert_eq!(one, [f2]);
}

/// Allocations as the backends report them come back exactly: 512-byte blocks, Windows
/// clusters, NTFS's 8-byte multiples for data inside the MFT record, and past 2 TiB.
#[test]
fn allocations_round_trip() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    for alloc in [
        0,
        512,
        4096,
        65536 * 3,
        104,
        8,
        2 << 40,
        (2 << 40) + 4096,
        u64::MAX,
    ] {
        let m = Meta {
            alloc,
            ..file_meta(1, alloc, 1)
        };
        let f = t.add_file(root, b"f", &m, false, None);
        assert_eq!(t.own(f).alloc, alloc);
    }
}

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
fn a_reused_slot_has_no_class_or_protection() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let d = t.add_dir(root, b"target", &dir_meta());
    let f = add_file(&mut t, d, "f", 1);
    let class = Class {
        rule: crate::RuleIx(3),
        safety: crate::Safety::Regenerable,
        degraded: None,
        flags: 0,
    };
    t.set_class(d, Some(class));
    t.set_protection(root, true, false);
    t.set_protection(d, false, true);
    assert_eq!(t.class_in_effect(f, |_| false), Some((d, class)));
    assert!(t.is_protected(root) && !t.is_protected(f));
    t.detach(d);
    t.free_step(usize::MAX, |_| {});
    assert_eq!(t.classes().count(), 0);
    let e = t.add_dir(root, b"e", &dir_meta());
    assert!([d.0, f.0].contains(&e.0), "slot reused");
    assert_eq!(t.class(e), None);
    assert!(t.is_protected(e), "only the root's mark applies");
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
fn errors_and_shared_bytes_survive_packing_and_slot_reuse() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let errs = [
        ErrKind::PermissionDenied,
        ErrKind::NotFound,
        ErrKind::Stalled,
        ErrKind::Other,
        ErrKind::Os(0),
        ErrKind::Os(5),
        ErrKind::Os(249),
        ErrKind::Os(250),
        ErrKind::Os(65531),
        ErrKind::Os(1 << 20),
        ErrKind::Os(-3),
    ];
    let files: Vec<Ix> = errs
        .iter()
        .enumerate()
        .map(|(i, &e)| {
            let m = file_meta(1, 100 + i as u64, 1);
            let f = t.add_file(root, format!("e{i}").as_bytes(), &m, false, Some(e));
            let own = t.own(f);
            t.propagate_add(root, &own);
            f
        })
        .collect();
    for (&f, &e) in files.iter().zip(&errs) {
        assert_eq!(t.error(f), Some(e));
    }
    assert_eq!(t.dev_ino(files[3]), (1, 103));
    // Allocations a block count cannot hold: not whole blocks, 2 TiB and more.
    let allocs = [0, 512, 4096, 100, (1u64 << 41) - 1024, 1 << 41, 1 << 50];
    for (&f, &a) in files.iter().zip(&allocs) {
        t.update_meta(
            f,
            &Meta {
                alloc: a,
                ..file_meta(1, 1, 1)
            },
        );
        assert_eq!(t.raw_size(f), (1, a));
    }
    t.add_shared(files[0], 70, true);
    t.set_measured(files[1], 9, false);
    assert_eq!((t.shared_here(files[0]), t.excl(files[1])), (70, 9));
    assert_eq!(t.own(files[0]).shared_out, 70);
    // Reused slots start clean.
    for &f in &files {
        t.detach(f);
    }
    t.free_step(usize::MAX, |_| {});
    let g = add_file(&mut t, root, "g", 1);
    assert_eq!((t.error(g), t.shared_here(g), t.excl(g)), (None, 0, 0));
    let h = add_file(&mut t, root, "h", 1);
    assert_eq!(
        t.raw_size(h),
        (1, 1),
        "a reused slot drops its big allocation"
    );
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

/// The nearest match covers a node, unless a match above it stops the descent: then the
/// highest such match does.
#[test]
fn the_nearest_match_is_in_effect_unless_one_above_stops_the_descent() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let outer = add_dir(&mut t, root, "outer");
    let mid = add_dir(&mut t, outer, "mid");
    let inner = add_dir(&mut t, mid, "inner");
    let f = add_file(&mut t, inner, "f", 1);
    let class = |rule| Class {
        rule: crate::RuleIx(rule),
        safety: crate::Safety::Safe,
        degraded: None,
        flags: 0,
    };
    t.set_class(outer, Some(class(1)));
    t.set_class(inner, Some(class(2)));
    let none = |_: &Class| false;
    assert_eq!(t.class_in_effect(f, none), Some((inner, class(2))));
    assert_eq!(t.class_in_effect(mid, none), Some((outer, class(1))));
    assert_eq!(t.class_in_effect(root, none), None);
    // Rule 1 stops the descent: it covers what is inside it, the nearer match too.
    let one = |c: &Class| c.rule.0 == 1;
    assert_eq!(t.class_in_effect(f, one), Some((outer, class(1))));
    // Both stop it: the highest wins.
    let all = |_: &Class| true;
    assert_eq!(t.class_in_effect(f, all), Some((outer, class(1))));
    // A match below that stops the descent does not lift one above that does not.
    let two = |c: &Class| c.rule.0 == 2;
    assert_eq!(t.class_in_effect(f, two), Some((inner, class(2))));
    // One step down at a time gives the same.
    let below = t.class_below(t.class_in_effect(mid, one), inner, one);
    assert_eq!(below, Some((outer, class(1))));
    assert_eq!(t.class_below(None, inner, one), Some((inner, class(2))));
}
