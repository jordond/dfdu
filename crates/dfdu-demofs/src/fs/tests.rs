use super::*;
use crate::test_support::{SMALL, small};

#[test]
fn every_error_has_its_errno() {
    let pairs = [
        (FsError::NoEnt, Errno::ENOENT),
        (FsError::NotDir, Errno::ENOTDIR),
        (FsError::IsDir, Errno::EISDIR),
        (FsError::NotEmpty, Errno::ENOTEMPTY),
        (FsError::Exist, Errno::EEXIST),
        (FsError::Inval, Errno::EINVAL),
        (FsError::Denied, Errno::EACCES),
        (FsError::Io, Errno::EIO),
        (FsError::TooBig, Errno::EFBIG),
    ];
    for (e, want) in pairs {
        assert_eq!(errno(e), want, "{e:?}");
    }
}

#[test]
fn inode_numbers_are_nodes_plus_one() {
    assert_eq!(ino(0), INodeNo(1));
    assert_eq!(node(INodeNo(1)), 0);
    assert_eq!(node(ino(41)), 41);
    // Inode 0 is never asked for; it would be the root rather than wrap.
    assert_eq!(node(INodeNo(0)), 0);
}

#[test]
fn attributes_follow_the_tree() {
    let t = small();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    let a = attr(&t, iso).unwrap();
    assert_eq!(
        (a.ino, a.kind, a.size),
        (ino(iso), FileType::RegularFile, 1 << 20)
    );
    assert_eq!((a.blocks, a.perm, a.nlink), ((1 << 20) / 512, 0o644, 1));
    assert_eq!((a.uid, a.gid), (1000, 1000));
    let secs = a.mtime.duration_since(UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(i64::try_from(secs).unwrap(), t.nodes[iso].mtime);
    let sparse = attr(&t, t.resolve("sparse.img").unwrap()).unwrap();
    assert_eq!((sparse.size, sparse.blocks), (10 << 30, 8));
    // A directory links to itself, its parent and its subdirectories.
    let dl = attr(&t, t.resolve("Downloads").unwrap()).unwrap();
    assert_eq!((dl.kind, dl.nlink, dl.size), (FileType::Directory, 3, 4096));
    let link = attr(&t, t.resolve("dev/shop/link").unwrap()).unwrap();
    assert_eq!(
        (link.kind, link.size, link.blocks),
        (FileType::Symlink, 12, 0)
    );
    let pkg = attr(&t, t.resolve("dev/shop/package.json").unwrap()).unwrap();
    assert_eq!(pkg.nlink, 2);
    assert_eq!(attr(&t, 10_000), Err(FsError::NoEnt));
}

#[test]
fn a_faulty_file_fails_to_stat_and_a_faulty_directory_does_not() {
    let mut t = small();
    let iso = t.resolve("Downloads/a.iso").unwrap();
    t.nodes[iso].fault = Some(crate::Fault::Io);
    assert_eq!(stat(&t, iso), Err(FsError::Io));
    assert!(attr(&t, iso).is_ok());
    let locked = t.resolve("Library/locked").unwrap();
    assert!(stat(&t, locked).is_ok());
    let b = t.resolve("Downloads/b.iso").unwrap();
    t.unlink(t.resolve("Downloads").unwrap(), b"b.iso", 0)
        .unwrap();
    assert_eq!(stat(&t, b), Err(FsError::NoEnt));
}

#[test]
fn a_listing_resumes_after_its_offset() {
    let mut t = small();
    let shop = t.resolve("dev/shop").unwrap();
    let all = listing(&t, shop, 0).unwrap();
    let names: Vec<_> = all.iter().map(|l| l.name).collect();
    assert_eq!(
        names,
        [
            &b"."[..],
            b"..",
            b"package.json",
            b"node_modules",
            b"link",
            b"hard"
        ]
    );
    assert_eq!((all[0].cookie, all[0].ino), (1, ino(shop)));
    assert_eq!((all[1].cookie, all[1].ino), (2, ino(t.parent(shop))));
    let kinds: Vec<_> = all.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        [
            FileType::Directory,
            FileType::Directory,
            FileType::RegularFile,
            FileType::Directory,
            FileType::Symlink,
            FileType::RegularFile,
        ]
    );
    let after_dots = listing(&t, shop, 2).unwrap();
    assert_eq!(after_dots, listing(&t, shop, 0).unwrap()[2..]);
    // Resumed after an entry that has since gone, nothing is skipped or repeated.
    let resume = all[3].cookie;
    let rest: Vec<_> = all[4..].iter().map(|l| l.name.to_vec()).collect();
    t.unlink(shop, b"package.json", 0).unwrap();
    let pkg = t.resolve("dev/shop/hard").unwrap();
    t.rename(shop, b"hard", shop, b"hard2", false, false, 0)
        .unwrap();
    let names: Vec<_> = listing(&t, shop, resume)
        .unwrap()
        .iter()
        .map(|l| l.name.to_vec())
        .collect();
    assert_eq!(names, [rest[0].clone(), b"hard2".to_vec()]);
    assert_eq!(t.resolve("dev/shop/hard2"), Ok(pkg));
    assert_eq!(listing(&t, shop, u64::MAX).unwrap().len(), 0);
    assert_eq!(listing(&t, pkg, 0), Err(FsError::NotDir));
}

#[test]
fn the_root_lists_itself_as_its_parent() {
    let t = small();
    let root = listing(&t, 0, 0).unwrap();
    assert_eq!((root[0].ino, root[1].ino), (INodeNo(1), INodeNo(1)));
}

#[test]
fn free_space_grows_as_files_go_and_stays_within_the_disk() {
    let f = Fixture::parse(SMALL).unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    assert_eq!(fs.total, 1_000_000_000_000);
    assert_eq!(fs.free(&fs.tree()), 200_000_000_000);
    {
        let mut t = fs.tree();
        let dl = t.resolve("Downloads").unwrap();
        t.unlink(dl, b"b.iso", 0).unwrap();
    }
    assert_eq!(fs.free(&fs.tree()), 200_000_000_000 + (1 << 20));
    // No more than the disk, however much a fixture says is free.
    let f =
        Fixture::parse("now = \"2026-10-01\"\n[disk]\ntotal = \"1GB\"\nfree = \"2GB\"").unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    assert_eq!(fs.free(&fs.tree()), 1_000_000_000);
}

#[test]
fn the_clock_starts_at_the_fixtures_now() {
    let f = Fixture::parse(SMALL).unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    let now = f.now().unwrap();
    assert!((now..now + 5).contains(&fs.time()));
}

#[test]
fn a_disk_that_is_not_a_size_is_refused() {
    let f = Fixture::parse("now = \"2026-10-01\"\n[disk]\ntotal = \"big\"").unwrap();
    let e = DemoFs::new(&f, Tree::new(0, (0, 0))).err().unwrap();
    assert!(format!("{e:#}").contains("disk total"), "{e:#}");
}
