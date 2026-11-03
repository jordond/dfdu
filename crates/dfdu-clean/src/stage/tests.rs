use super::*;

#[test]
fn rename_noreplace_refuses_to_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    fs::write(&a, b"a").unwrap();
    fs::write(&b, b"b").unwrap();
    let e = rename_noreplace(&a, &b).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&b).unwrap(), b"b");
    fs::remove_file(&b).unwrap();
    rename_noreplace(&a, &b).unwrap();
    assert_eq!(fs::read(&b).unwrap(), b"a");
}

#[test]
fn put_back_goes_next_to_what_is_there_now() {
    let tmp = tempfile::tempdir().unwrap();
    let orig = tmp.path().join("x");
    let staged = tmp.path().join("staged");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("f"), b"old").unwrap();
    fs::create_dir(&orig).unwrap();
    fs::write(orig.join("f"), b"new").unwrap();
    let to = put_back(&staged, &id_of(&staged), &orig).unwrap();
    assert_eq!(to, tmp.path().join("x.restored-1"));
    assert_eq!(fs::read(orig.join("f")).unwrap(), b"new");
    assert_eq!(fs::read(to.join("f")).unwrap(), b"old");
}

/// The identity of what is at `path` now.
fn id_of(path: &Path) -> Identity {
    let m = meta(path).unwrap();
    Identity {
        dev: m.dev,
        ino: m.ino,
        kind: m.kind,
        apparent: m.apparent,
        mtime: m.mtime,
    }
}

/// A plan item for the file or directory at `path`, with its identity as it is now.
#[cfg(unix)]
fn item_at(path: &Path) -> PlanItem {
    let m = meta(path).unwrap();
    let mut tree = dfdu_tree::Tree::new();
    let root = tree.new_root(b"/x", &m, false);
    PlanItem {
        node: tree.node_ref(root),
        path: path.into(),
        identity: id_of(path),
        bytes: 1,
        inside: std::sync::Arc::default(),
    }
}

/// An undo puts the staged item back where it was.
#[cfg(unix)]
#[test]
fn a_staged_item_is_put_back() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f");
    fs::write(&f, b"x").unwrap();
    let item = item_at(&f);
    let mut s = Stager::new("s".into(), None);
    let to = s.target(&item, tmp.path()).unwrap();
    s.stage(&f, &item.identity, &to).unwrap();
    assert_eq!(put_back(&to, &item.identity, &f).unwrap(), f);
    assert_eq!(fs::read(&f).unwrap(), b"x");
    assert!(!to.exists());
}

/// Something else put at the staged item's path, or the session directory replaced by another
/// holding a file of the same name: the undo is refused and nothing moves.
#[cfg(unix)]
#[test]
fn a_swapped_staged_item_is_not_put_back() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f");
    fs::write(&f, b"x").unwrap();
    let item = item_at(&f);
    let mut s = Stager::new("s".into(), None);
    let to = s.target(&item, tmp.path()).unwrap();
    s.stage(&f, &item.identity, &to).unwrap();

    let aside = tmp.path().join("aside");
    fs::rename(&to, &aside).unwrap();
    fs::write(&to, b"planted").unwrap();
    let e = put_back(&to, &item.identity, &f).unwrap_err();
    assert!(e.to_string().contains("no longer"), "{e}");
    assert!(!f.exists());
    assert_eq!(fs::read(&to).unwrap(), b"planted");
    fs::remove_file(&to).unwrap();
    fs::rename(&aside, &to).unwrap();

    let session = to.parent().unwrap();
    let moved = tmp.path().join("moved");
    fs::rename(session, &moved).unwrap();
    fs::create_dir(session).unwrap();
    fs::write(&to, b"planted").unwrap();
    assert!(put_back(&to, &item.identity, &f).is_err());
    assert!(!f.exists());
    assert_eq!(fs::read(moved.join("1-f")).unwrap(), b"x");
    // A symlink to where the session went is not followed either.
    fs::remove_dir_all(session).unwrap();
    std::os::unix::fs::symlink(&moved, session).unwrap();
    assert!(put_back(&to, &item.identity, &f).is_err());
    assert!(!f.exists());
}

/// A root dfdu puts back only from a staging directory of root's; any other process from any
/// it can move out of.
#[cfg(unix)]
#[test]
fn root_trusts_only_its_own_staging_directories() {
    assert!(at::trusted(0, 0));
    assert!(!at::trusted(0, 501));
    assert!(at::trusted(501, 501));
    assert!(at::trusted(501, 0));
}

#[cfg(unix)]
#[test]
fn an_item_is_staged_into_the_session_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f");
    fs::write(&f, b"x").unwrap();
    let item = item_at(&f);
    let mut s = Stager::new("s".into(), None);
    let to = s.target(&item, tmp.path()).unwrap();
    assert_eq!(to, tmp.path().join(".dfdu-stage/s/1-f"));
    s.stage(&f, &item.identity, &to).unwrap();
    assert!(!f.exists());
    assert_eq!(fs::read(&to).unwrap(), b"x");
}

/// A `.dfdu-stage` planted as a symlink (in `/tmp`, say) is not staged into, nor followed.
#[cfg(unix)]
#[test]
fn a_planted_staging_symlink_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir_in(tmp.path()).unwrap();
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), root.join(".dfdu-stage")).unwrap();
    let f = root.join("f");
    fs::write(&f, b"x").unwrap();
    let mut s = Stager::new("s".into(), None);
    assert!(s.target(&item_at(&f), &root).is_err());
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    // A file there is refused too.
    fs::remove_file(root.join(".dfdu-stage")).unwrap();
    fs::write(root.join(".dfdu-stage"), b"").unwrap();
    assert!(s.target(&item_at(&f), &root).is_err());
}

/// The item replaced since it was verified, or a directory above it swapped for a symlink to
/// where it went: nothing is staged.
#[cfg(unix)]
#[test]
fn a_swapped_item_is_not_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    fs::create_dir(&a).unwrap();
    let f = a.join("f");
    fs::write(&f, b"x").unwrap();
    let item = item_at(&f);
    let mut s = Stager::new("s".into(), None);
    let to = s.target(&item, tmp.path()).unwrap();

    fs::remove_file(&f).unwrap();
    fs::write(&f, b"other").unwrap();
    let e = s.stage(&f, &item.identity, &to).unwrap_err();
    assert!(e.to_string().contains("no longer"), "{e}");
    assert_eq!(fs::read(&f).unwrap(), b"other");

    let g = a.join("g");
    fs::write(&g, b"y").unwrap();
    let item = item_at(&g);
    let to = s.target(&item, tmp.path()).unwrap();
    fs::rename(&a, tmp.path().join("b")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("b"), &a).unwrap();
    assert!(s.stage(&g, &item.identity, &to).is_err());
    assert_eq!(fs::read(tmp.path().join("b/g")).unwrap(), b"y");
    assert!(!to.exists());
}

/// The staging directory renamed away and replaced after it was made: the item goes to the
/// directory that was checked, not to what its path names now.
#[cfg(unix)]
#[test]
fn the_staging_directory_is_used_as_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f");
    fs::write(&f, b"x").unwrap();
    let item = item_at(&f);
    let mut s = Stager::new("s".into(), None);
    let to = s.target(&item, tmp.path()).unwrap();
    let stage = tmp.path().join(".dfdu-stage");
    fs::rename(&stage, tmp.path().join("moved")).unwrap();
    let decoy = tempfile::tempdir_in(tmp.path()).unwrap();
    fs::create_dir(decoy.path().join("s")).unwrap();
    std::os::unix::fs::symlink(decoy.path(), &stage).unwrap();
    s.stage(&f, &item.identity, &to).unwrap();
    assert_eq!(fs::read(tmp.path().join("moved/s/1-f")).unwrap(), b"x");
    assert_eq!(fs::read_dir(decoy.path().join("s")).unwrap().count(), 0);
}

/// A target this stager did not give is refused.
#[test]
fn only_its_own_targets_are_staged_to() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f");
    fs::write(&f, b"x").unwrap();
    let id = Identity {
        dev: 0,
        ino: 0,
        kind: dfdu_tree::Kind::File,
        apparent: 1,
        mtime: 0,
    };
    let s = Stager::new("s".into(), None);
    assert!(s.stage(&f, &id, &tmp.path().join("g")).is_err());
    assert!(f.exists());
}
