use super::*;
use crate::test_support::small;

mod disk;
mod listing;

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
