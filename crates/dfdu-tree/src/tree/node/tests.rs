use super::super::test_util::{add_file, dir_meta, file_meta};
use super::*;
use crate::Meta;

#[test]
fn allocations_pack_into_blocks_when_they_can() {
    assert_eq!(pack_alloc(0), Some(0));
    assert_eq!(pack_alloc(512), Some(1));
    assert_eq!(pack_alloc(4096 * 3), Some(24));
    assert_eq!(pack_alloc(104), None, "not whole blocks");
    assert_eq!(
        pack_alloc(u64::from(ALLOC_BIG - 1) * 512),
        Some(ALLOC_BIG - 1)
    );
    assert_eq!(
        pack_alloc(u64::from(ALLOC_BIG) * 512),
        None,
        "the marker is not a count"
    );
    assert_eq!(pack_alloc(1 << 50), None);
}

#[test]
fn errors_pack_into_a_byte_when_they_can() {
    assert_eq!(pack_err(ErrKind::PermissionDenied), Some(ERR_PERMISSION));
    assert_eq!(pack_err(ErrKind::NotFound), Some(ERR_NOT_FOUND));
    assert_eq!(pack_err(ErrKind::Stalled), Some(ERR_STALLED));
    assert_eq!(pack_err(ErrKind::Other), Some(ERR_OTHER));
    assert_eq!(pack_err(ErrKind::Os(0)), Some(ERR_OS));
    assert_eq!(pack_err(ErrKind::Os(249)), Some(254));
    assert_eq!(pack_err(ErrKind::Os(250)), None, "would be the marker");
    assert_eq!(pack_err(ErrKind::Os(256)), None);
    assert_eq!(pack_err(ErrKind::Os(-1)), None);
}

#[test]
fn flags_set_and_clear_one_bit() {
    let mut f = NodeFlags::default();
    f.set(NodeFlags::MOUNT_ROOT, true);
    f.set(NodeFlags::EXCEPTED, true);
    assert!(f.has(NodeFlags::MOUNT_ROOT) && f.has(NodeFlags::EXCEPTED));
    assert!(!f.has(NodeFlags::HARDLINK));
    f.set(NodeFlags::MOUNT_ROOT, false);
    assert!(!f.has(NodeFlags::MOUNT_ROOT) && f.has(NodeFlags::EXCEPTED));
}

#[test]
fn devices_are_numbered_once_each() {
    let mut t = Tree::new();
    let (a, b) = (t.dev_index(7), t.dev_index(9));
    assert_ne!(a, b);
    assert_eq!(t.dev_index(7), a);
    assert_eq!(t.dev_index(9), b);
    assert_eq!(t.devs, [7, 9]);
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
