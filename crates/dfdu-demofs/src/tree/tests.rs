use super::*;
use crate::test_util::{names, small};

mod rename;

/// A root with `a/` holding the files `f` (100 bytes) and `g`, and the empty directory `e/`.
pub(super) fn tiny() -> (Tree, [usize; 4]) {
    let mut t = Tree::new(0, (7, 8));
    let dir = t.insert(0, b"a", Kind::dir(), 1, 0o755).unwrap();
    let file = t.insert(dir, b"f", Kind::seeded(100, 1), 2, 0o644).unwrap();
    let other = t.insert(dir, b"g", Kind::seeded(10, 2), 3, 0o600).unwrap();
    let empty = t.insert(0, b"e", Kind::dir(), 1, 0o755).unwrap();
    (t, [dir, file, other, empty])
}

#[test]
fn inserts_set_the_parent_owner_and_the_directory_time() {
    let (t, [dir, file, other, empty]) = tiny();
    assert_eq!(t.parent(dir), 0);
    assert_eq!(t.parent(0), 0);
    assert_eq!(t.parent(file), 0, "a file has no parent of its own");
    assert_eq!((t.nodes[file].uid, t.nodes[file].gid), (7, 8));
    assert_eq!(t.nodes[dir].mtime, 3, "the newest entry's time");
    assert_eq!(t.lookup(dir, b"g"), Ok(other));
    assert_eq!(t.resolve("/a//f"), Ok(file));
    assert_eq!(t.resolve(""), Ok(0));
    assert_eq!(t.resolve("a/f/x"), Err(FsError::NotDir));
    assert_eq!(t.resolve("a/x"), Err(FsError::NoEnt));
    assert_eq!(t.subdirs(0), 2);
    assert_eq!(t.subdirs(dir), 0);
    assert_eq!(t.subdirs(file), 0);
    assert_eq!(names(&t, 0), [b"a", b"e"]);
    assert_eq!(t.list(file).unwrap_err(), FsError::NotDir);
    assert_eq!(t.list(empty).unwrap().len(), 0);
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
    let (mut t, [dir, file, other, empty]) = tiny();
    assert_eq!((t.size(dir), t.alloc(dir)), (DIR_SIZE, DIR_SIZE));
    assert_eq!((t.size(file), t.alloc(file)), (100, 4096));
    let link = t
        .insert(0, b"s", Kind::Symlink(b"a/f".to_vec()), 0, 0o777)
        .unwrap();
    assert_eq!((t.size(link), t.alloc(link)), (3, 0));
    assert_eq!(t.used(), 3 * DIR_SIZE + 2 * 4096);
    assert_eq!(t.apparent(), 2 * DIR_SIZE + 100 + 10 + 3);
    assert_eq!(t.live(), 6);
    t.unlink(dir, b"g", 9).unwrap();
    t.rmdir(0, b"e", 9).unwrap();
    assert_eq!(t.live(), 4);
    assert_eq!((t.size(other), t.alloc(other), t.size(empty)), (0, 0, 0));
    assert!(t.get(other).is_none() && t.get(99).is_none());
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
    let (mut t, [dir, file, _, empty]) = tiny();
    t.link(file, empty, b"f2").unwrap();
    assert_eq!(t.nodes[file].nlink, 2);
    assert_eq!(t.resolve("e/f2"), Ok(file));
    assert_eq!(t.link(dir, empty, b"a2"), Err(FsError::IsDir));
    assert_eq!(t.link(file, empty, b"f2"), Err(FsError::Exist));
    assert_eq!(t.link(file, file, b"x"), Err(FsError::NotDir));
    assert_eq!(t.link(99, empty, b"x"), Err(FsError::NoEnt));
    assert_eq!(t.nodes[file].nlink, 2);
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
