//! Against the fixture of `scripts/btrfs-fixture.sh`, whose mount point is in `DUT_BTRFS_DIR`.
//! Skipped (passing) when the variable is not set.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn fixture() -> Option<PathBuf> {
    std::env::var_os("DUT_BTRFS_DIR").map(PathBuf::from)
}

#[test]
fn subvolume_records() {
    let Some(d) = fixture() else { return };
    assert!(dut_sys::is_btrfs(&d).unwrap());
    let top = dut_sys::subvol_info(&d).unwrap().expect("top level");
    assert_eq!(top.id, 5);
    let sub = dut_sys::subvol_info(&d.join("sub")).unwrap().expect("sub");
    assert_eq!(
        (sub.name.as_slice(), sub.read_only, sub.parent_uuid),
        (&b"sub"[..], false, None)
    );
    let ro = dut_sys::subvol_info(&d.join("snap-ro"))
        .unwrap()
        .expect("snap-ro");
    assert!(ro.read_only);
    assert_eq!(ro.parent_uuid, Some(sub.uuid));
    let rw = dut_sys::subvol_info(&d.join("snap-rw"))
        .unwrap()
        .expect("snap-rw");
    assert!(!rw.read_only);
    assert_eq!(rw.parent_uuid, Some(sub.uuid));
    let nested = dut_sys::subvol_info(&d.join("sub/nested"))
        .unwrap()
        .expect("nested");
    assert_eq!(nested.parent_id, sub.id);
    assert_eq!(dut_sys::subvol_info(&d.join("plain")).unwrap(), None);
    // A snapshot does not include nested subvolumes: their place is an empty directory.
    assert_eq!(
        dut_sys::subvol_info(&d.join("snap-rw/nested")).unwrap(),
        None
    );
}

#[test]
fn subvolumes_share_the_mount_and_mounts_do_not() {
    let Some(d) = fixture() else { return };
    let id = |p: &str| dut_sys::mount_id(&d.join(p)).unwrap().expect("mount id");
    let top = id("");
    for p in ["plain", "sub", "sub/nested", "snap-ro", "snap-rw"] {
        assert_eq!(id(p), top, "{p}");
    }
    assert_ne!(id("mnt"), top, "the second mount of sub");
}

fn extents(p: &std::path::Path) -> Vec<dut_sys::Extent> {
    dut_sys::fiemap(std::fs::File::open(p).unwrap()).unwrap()
}

#[test]
fn fiemap_reports_sharing_compression_and_inline() {
    use dut_sys::Extent;
    let Some(d) = fixture() else { return };
    let p = d.join("plain");
    let len = |v: &[Extent]| v.iter().map(|e| e.length).sum::<u64>();
    // Copied by reflink twice: every extent shared.
    let random = extents(&p.join("random.bin"));
    assert_eq!(len(&random), 16 << 20);
    assert!(
        random.iter().all(|e| e.flags & Extent::SHARED != 0),
        "{random:?}"
    );
    // 4 MiB of the copy was overwritten: that part is not shared.
    let copy = extents(&p.join("reflink.bin"));
    let unshared: u64 = copy
        .iter()
        .filter(|e| e.flags & Extent::SHARED == 0)
        .map(|e| e.length)
        .sum();
    assert_eq!(unshared, 4 << 20);
    // Compressed: encoded extents with their uncompressed length.
    let text = extents(&p.join("text.txt"));
    assert!(text.iter().all(|e| e.flags & Extent::ENCODED != 0));
    assert_eq!(len(&text), 48 << 20);
    assert!(
        text.len() > 256,
        "more extents than one batch: {}",
        text.len()
    );
    let tiny = extents(&p.join("tiny.txt"));
    assert_eq!(tiny.len(), 1);
    assert!(tiny[0].flags & Extent::DATA_INLINE != 0);
    assert!(extents(&p.join("sparse.bin")).is_empty());
    let pre = extents(&p.join("prealloc.bin"));
    assert!(pre.iter().all(|e| e.flags & Extent::UNWRITTEN != 0));
}

#[test]
fn one_fsid_across_subvolumes() {
    let Some(d) = fixture() else { return };
    let id = |p: &str| dut_sys::btrfs_fsid(std::fs::File::open(d.join(p)).unwrap()).unwrap();
    let top = id("plain/random.bin");
    assert_eq!(id("sub/a.bin"), top);
    assert_eq!(id("snap-ro/a.bin"), top);
    assert_eq!(id("mnt/a.bin"), top);
    assert!(dut_sys::shares_extents(&d).unwrap());
    assert!(!dut_sys::shares_extents(std::path::Path::new("/proc")).unwrap());
}

#[test]
fn file_extent_items_as_root() {
    use dut_sys::{FileExtent, FileExtentKind};
    let Some(d) = fixture() else { return };
    if !dut_sys::is_privileged() {
        return;
    }
    let items = |p: &str| -> Vec<FileExtent> {
        let f = std::fs::File::open(d.join(p)).unwrap();
        use std::os::unix::fs::MetadataExt;
        let ino = f.metadata().unwrap().ino();
        dut_sys::file_extents(&f, ino).unwrap()
    };
    if std::env::var_os("DUT_SHOW_EXTENTS").is_some() {
        for p in ["plain/bookend.bin", "plain/text.txt", "plain/tiny.txt"] {
            eprintln!("{p}: {:#?}", &items(p)[..items(p).len().min(6)]);
        }
    }
    let tiny = items("plain/tiny.txt");
    assert_eq!(tiny.len(), 1);
    assert_eq!(
        (tiny[0].kind, tiny[0].disk_num_bytes),
        (FileExtentKind::Inline, 5)
    );
    let text = items("plain/text.txt");
    assert!(text.iter().all(|e| e.compression == 3), "zstd");
    let on_disk: u64 = text.iter().map(|e| e.disk_num_bytes).sum();
    assert!(on_disk < 4 << 20, "compressed size {on_disk}");
    assert_eq!(text.iter().map(|e| e.num_bytes).sum::<u64>(), 48 << 20);
    let pre = items("plain/prealloc.bin");
    assert!(pre.iter().all(|e| e.kind == FileExtentKind::Prealloc));
    // No quota rescan is needed for a fresh fixture.
    let top = std::fs::File::open(&d).unwrap();
    let sub = dut_sys::subvol_info(&d.join("sub")).unwrap().unwrap();
    let (rfer, excl) = dut_sys::qgroup_usage(&top, sub.id)
        .unwrap()
        .expect("quotas are on");
    assert!(rfer >= 10 << 20 && excl < 1 << 20, "{rfer} {excl}");
    let (profile, devices) = dut_sys::data_profile(&top).unwrap();
    assert_eq!((profile, devices), (dut_sys::Profile::Single, 1));
}

/// The fixture's second filesystem, at `$DUT_BTRFS_DIR-dup`, has the DUP data profile: every
/// byte is stored twice. Reading the profile needs no privileges.
#[test]
fn dup_profile() {
    let Some(d) = fixture() else { return };
    let mut dup = d.into_os_string();
    dup.push("-dup");
    let (p, devices) = dut_sys::data_profile(std::fs::File::open(&dup).unwrap()).unwrap();
    assert_eq!((p, devices), (dut_sys::Profile::Dup, 1));
    assert_eq!(p.raw_ratio(devices), 2.0);
}
