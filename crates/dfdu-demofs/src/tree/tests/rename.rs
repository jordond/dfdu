//! `rename`, with and without `RENAME_NOREPLACE` and `RENAME_EXCHANGE`.

use super::super::*;
use super::tiny;
use crate::test_support::small;

#[test]
fn rename_moves_replaces_and_refuses_as_rename2_does() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    let root = 0;
    // NOREPLACE onto an existing name.
    assert_eq!(
        t.rename(dl, b"a.iso", dl, b"b.iso", true, false, 0),
        Err(FsError::Exist)
    );
    // A directory into itself.
    let old = t.resolve("Downloads/old").unwrap();
    assert_eq!(
        t.rename(root, b"Downloads", old, b"x", false, false, 0),
        Err(FsError::Inval)
    );
    // Replacing a file.
    let a = t.resolve("Downloads/a.iso").unwrap();
    t.rename(dl, b"a.iso", dl, b"b.iso", false, false, 0)
        .unwrap();
    assert_eq!(t.resolve("Downloads/b.iso").unwrap(), a);
    assert_eq!(t.resolve("Downloads/a.iso"), Err(FsError::NoEnt));
    // Moving a directory updates its parent.
    let trash = t.insert(root, b"Trash", Kind::dir(), 0, 0o755).unwrap();
    t.rename(dl, b"old", trash, b"old", true, false, 0).unwrap();
    assert_eq!(t.parent(old), trash);
    // Exchange.
    let shop = t.resolve("dev/shop").unwrap();
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    let link = t.resolve("dev/shop/link").unwrap();
    t.rename(shop, b"package.json", shop, b"link", false, true, 0)
        .unwrap();
    assert_eq!(t.resolve("dev/shop/link").unwrap(), pkg);
    assert_eq!(t.resolve("dev/shop/package.json").unwrap(), link);
}

#[test]
fn rename_onto_a_directory_needs_an_empty_one() {
    let (mut t, [dir, file, _, empty]) = tiny();
    // A file onto a directory, a directory onto a file or onto a non-empty directory.
    assert_eq!(
        t.rename(dir, b"f", 0, b"e", false, false, 4),
        Err(FsError::IsDir)
    );
    assert_eq!(
        t.rename(0, b"e", dir, b"f", false, false, 4),
        Err(FsError::NotDir)
    );
    assert_eq!(
        t.rename(0, b"e", 0, b"a", false, false, 4),
        Err(FsError::NotEmpty)
    );
    // Onto an empty one, which goes.
    let moved = t.insert(dir, b"d", Kind::dir(), 0, 0o755).unwrap();
    t.rename(dir, b"d", 0, b"e", false, false, 4).unwrap();
    assert_eq!(t.resolve("e"), Ok(moved));
    assert_eq!(t.parent(moved), 0);
    assert!(t.get(empty).is_none());
    assert_eq!((t.nodes[0].mtime, t.nodes[dir].mtime), (4, 4));
    // A name onto itself, and a missing source or target directory.
    t.rename(dir, b"f", dir, b"f", true, false, 5).unwrap();
    assert_eq!(t.resolve("a/f"), Ok(file));
    assert_eq!(
        t.rename(dir, b"x", dir, b"y", false, false, 5),
        Err(FsError::NoEnt)
    );
    assert_eq!(
        t.rename(dir, b"f", file, b"y", false, false, 5),
        Err(FsError::NotDir)
    );
}

#[test]
fn exchange_swaps_directories_and_their_parents() {
    let (mut t, [dir, file, _, empty]) = tiny();
    assert_eq!(
        t.rename(dir, b"f", dir, b"x", false, true, 0),
        Err(FsError::NoEnt)
    );
    // Neither may end up below itself.
    let inner = t.insert(dir, b"inner", Kind::dir(), 0, 0o755).unwrap();
    assert_eq!(
        t.rename(0, b"a", dir, b"inner", false, true, 0),
        Err(FsError::Inval)
    );
    assert_eq!(
        t.rename(dir, b"inner", 0, b"a", false, true, 0),
        Err(FsError::Inval)
    );
    t.rename(0, b"e", dir, b"inner", false, true, 6).unwrap();
    assert_eq!(t.resolve("e"), Ok(inner));
    assert_eq!(t.resolve("a/inner"), Ok(empty));
    assert_eq!((t.parent(inner), t.parent(empty)), (0, dir));
    assert_eq!(t.resolve("a/f"), Ok(file));
}
