use super::*;
use crate::test_support::{names, small};

/// A root with `a/` holding the files `f` (100 bytes) and `g`, and the empty directory `e/`.
fn tiny() -> (Tree, [usize; 4]) {
    let mut t = Tree::new(0, (7, 8));
    let a = t.insert(0, b"a", Kind::dir(), 1, 0o755).unwrap();
    let f = t.insert(a, b"f", Kind::seeded(100, 1), 2, 0o644).unwrap();
    let g = t.insert(a, b"g", Kind::seeded(10, 2), 3, 0o600).unwrap();
    let e = t.insert(0, b"e", Kind::dir(), 1, 0o755).unwrap();
    (t, [a, f, g, e])
}

#[test]
fn inserts_set_the_parent_owner_and_the_directory_time() {
    let (t, [a, f, g, e]) = tiny();
    assert_eq!(t.parent(a), 0);
    assert_eq!(t.parent(0), 0);
    assert_eq!(t.parent(f), 0, "a file has no parent of its own");
    assert_eq!((t.nodes[f].uid, t.nodes[f].gid), (7, 8));
    assert_eq!(t.nodes[a].mtime, 3, "the newest entry's time");
    assert_eq!(t.lookup(a, b"g"), Ok(g));
    assert_eq!(t.resolve("/a//f"), Ok(f));
    assert_eq!(t.resolve(""), Ok(0));
    assert_eq!(t.resolve("a/f/x"), Err(FsError::NotDir));
    assert_eq!(t.resolve("a/x"), Err(FsError::NoEnt));
    assert_eq!(t.subdirs(0), 2);
    assert_eq!(t.subdirs(a), 0);
    assert_eq!(t.subdirs(f), 0);
    assert_eq!(names(&t, 0), [b"a", b"e"]);
    assert_eq!(t.list(f).unwrap_err(), FsError::NotDir);
    assert!(t.list(e).unwrap().is_empty());
}

#[test]
fn names_that_are_not_one_component_are_refused() {
    let (mut t, _) = tiny();
    for bad in [&b""[..], b".", b"..", b"x/y"] {
        assert_eq!(t.insert(0, bad, Kind::dir(), 0, 0o755), Err(FsError::Inval));
    }
}

#[test]
fn a_failed_insert_leaves_nothing_behind() {
    let mut t = small();
    let nodes = t.nodes.len();
    let file = t.resolve("Downloads/a.iso").unwrap();
    assert_eq!(
        t.insert(file, b"x", Kind::dir(), 0, 0o755),
        Err(FsError::NotDir)
    );
    assert_eq!(
        t.insert(0, b"Downloads", Kind::dir(), 0, 0o755),
        Err(FsError::Exist)
    );
    assert_eq!(t.nodes.len(), nodes);
}

#[test]
fn sizes_allocation_and_counts() {
    let (mut t, [a, f, g, e]) = tiny();
    assert_eq!((t.size(a), t.alloc(a)), (DIR_SIZE, DIR_SIZE));
    assert_eq!((t.size(f), t.alloc(f)), (100, 4096));
    let s = t
        .insert(0, b"s", Kind::Symlink(b"a/f".to_vec()), 0, 0o777)
        .unwrap();
    assert_eq!((t.size(s), t.alloc(s)), (3, 0));
    assert_eq!(t.used(), 3 * DIR_SIZE + 2 * 4096);
    assert_eq!(t.apparent(), 2 * DIR_SIZE + 100 + 10 + 3);
    assert_eq!(t.live(), 6);
    t.unlink(a, b"g", 9).unwrap();
    t.rmdir(0, b"e", 9).unwrap();
    assert_eq!(t.live(), 4);
    assert_eq!((t.size(g), t.alloc(g), t.size(e)), (0, 0, 0));
    assert!(t.get(g).is_none() && t.get(99).is_none());
}

#[test]
fn unlink_rmdir_and_used_space() {
    let mut t = small();
    let used = t.used();
    let dl = t.resolve("Downloads").unwrap();
    assert_eq!(t.rmdir(dl, b"old", 0), Err(FsError::NotEmpty));
    assert_eq!(t.unlink(dl, b"old", 0), Err(FsError::IsDir));
    assert_eq!(t.rmdir(dl, b"a.iso", 0), Err(FsError::NotDir));
    assert_eq!(t.unlink(dl, b"missing", 0), Err(FsError::NoEnt));
    let old = t.resolve("Downloads/old").unwrap();
    t.unlink(old, b"a (1).iso", 5).unwrap();
    t.rmdir(dl, b"old", 5).unwrap();
    assert_eq!(t.resolve("Downloads/old"), Err(FsError::NoEnt));
    assert_eq!(t.used(), used - (1 << 20) - 4096);
    assert_eq!(t.nodes[dl].mtime, 5);
    // A file with two links stays until the last goes.
    let shop = t.resolve("dev/shop").unwrap();
    let pkg = t.resolve("dev/shop/package.json").unwrap();
    t.unlink(shop, b"package.json", 0).unwrap();
    assert!(t.get(pkg).is_some());
    t.unlink(shop, b"hard", 0).unwrap();
    assert!(t.get(pkg).is_none());
}

#[test]
fn links_go_to_files_under_new_names() {
    let (mut t, [a, f, _, e]) = tiny();
    t.link(f, e, b"f2").unwrap();
    assert_eq!(t.nodes[f].nlink, 2);
    assert_eq!(t.resolve("e/f2"), Ok(f));
    assert_eq!(t.link(a, e, b"a2"), Err(FsError::IsDir));
    assert_eq!(t.link(f, e, b"f2"), Err(FsError::Exist));
    assert_eq!(t.link(f, f, b"x"), Err(FsError::NotDir));
    assert_eq!(t.link(99, e, b"x"), Err(FsError::NoEnt));
    assert_eq!(t.nodes[f].nlink, 2);
}

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
    let (mut t, [a, f, _, e]) = tiny();
    // A file onto a directory, a directory onto a file or onto a non-empty directory.
    assert_eq!(
        t.rename(a, b"f", 0, b"e", false, false, 4),
        Err(FsError::IsDir)
    );
    assert_eq!(
        t.rename(0, b"e", a, b"f", false, false, 4),
        Err(FsError::NotDir)
    );
    assert_eq!(
        t.rename(0, b"e", 0, b"a", false, false, 4),
        Err(FsError::NotEmpty)
    );
    // Onto an empty one, which goes.
    let d = t.insert(a, b"d", Kind::dir(), 0, 0o755).unwrap();
    t.rename(a, b"d", 0, b"e", false, false, 4).unwrap();
    assert_eq!(t.resolve("e"), Ok(d));
    assert_eq!(t.parent(d), 0);
    assert!(t.get(e).is_none());
    assert_eq!((t.nodes[0].mtime, t.nodes[a].mtime), (4, 4));
    // A name onto itself, and a missing source or target directory.
    t.rename(a, b"f", a, b"f", true, false, 5).unwrap();
    assert_eq!(t.resolve("a/f"), Ok(f));
    assert_eq!(
        t.rename(a, b"x", a, b"y", false, false, 5),
        Err(FsError::NoEnt)
    );
    assert_eq!(
        t.rename(a, b"f", f, b"y", false, false, 5),
        Err(FsError::NotDir)
    );
}

#[test]
fn exchange_swaps_directories_and_their_parents() {
    let (mut t, [a, f, _, e]) = tiny();
    assert_eq!(
        t.rename(a, b"f", a, b"x", false, true, 0),
        Err(FsError::NoEnt)
    );
    // Neither may end up below itself.
    let inner = t.insert(a, b"inner", Kind::dir(), 0, 0o755).unwrap();
    assert_eq!(
        t.rename(0, b"a", a, b"inner", false, true, 0),
        Err(FsError::Inval)
    );
    assert_eq!(
        t.rename(a, b"inner", 0, b"a", false, true, 0),
        Err(FsError::Inval)
    );
    t.rename(0, b"e", a, b"inner", false, true, 6).unwrap();
    assert_eq!(t.resolve("e"), Ok(inner));
    assert_eq!(t.resolve("a/inner"), Ok(e));
    assert_eq!((t.parent(inner), t.parent(e)), (0, a));
    assert_eq!(t.resolve("a/f"), Ok(f));
}

#[test]
fn cookies_grow_and_stay_with_their_entries() {
    let mut t = small();
    let dl = t.resolve("Downloads").unwrap();
    let cookies = |t: &Tree| {
        t.list(dl)
            .unwrap()
            .iter()
            .map(|e| e.cookie)
            .collect::<Vec<_>>()
    };
    let before = cookies(&t);
    assert!(before.windows(2).all(|w| w[0] < w[1]));
    assert!(before[0] > 2, "1 and 2 are `.` and `..`");
    // Removing one keeps the others' cookies; a hard link in the same directory gets its own.
    t.unlink(dl, b"b.iso", 0).unwrap();
    let a = t.resolve("Downloads/a.iso").unwrap();
    t.link(a, dl, b"a-again.iso").unwrap();
    let after = cookies(&t);
    assert!(after.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        before.iter().filter(|c| after.contains(c)).count(),
        before.len() - 1
    );
}

#[test]
fn faults_map_to_their_errors() {
    assert_eq!(FsError::from(Fault::Denied), FsError::Denied);
    assert_eq!(FsError::from(Fault::Io), FsError::Io);
}
