use super::test_util::{add_dir, add_file, dir_meta, file_meta};
use super::*;
use crate::Meta;

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
