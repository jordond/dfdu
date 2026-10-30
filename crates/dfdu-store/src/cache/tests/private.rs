//! Who may read the cache, and which directories it refuses.

use std::os::unix::fs::PermissionsExt;

use super::*;

/// The directory created is the user's alone and the files are readable by the user only. A
/// directory that exists is used as it is, unless other users can write to it.
#[test]
fn files_are_private() {
    let (tmp, cache, slot) = setup();
    let (t, _) = tree_of(slot.key.root(), 2);
    save(&slot, &t, &header(now(), now()));
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(cache.dir()), 0o700);
    assert_eq!(mode(&slot.path), 0o600);
    fs::set_permissions(cache.dir(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Cache::at(cache.dir().to_path_buf()).is_ok());
    save(&slot, &t, &header(now(), now()));
    assert_eq!(mode(cache.dir()), 0o755, "not changed");
    for loose in [0o775, 0o757] {
        fs::set_permissions(cache.dir(), fs::Permissions::from_mode(loose)).unwrap();
        let e = Cache::at(cache.dir().to_path_buf()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().ends_with("is writable by other users"), "{e}");
        let e = slot
            .save(&header(now(), now()), |f| f(&t), &AtomicBool::new(false))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(mode(cache.dir()), loose, "not changed");
    }
    fs::set_permissions(cache.dir(), fs::Permissions::from_mode(0o700)).unwrap();
    let leftovers: Vec<_> = fs::read_dir(cache.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    drop(tmp);
}

/// A cache below a directory of another user is not used (`/` is root's).
#[test]
fn another_users_directory_is_not_used() {
    if euid() == 0 {
        return;
    }
    assert!(Cache::at(PathBuf::from("/dfdu-test-no-such-dir/scans")).is_err());
    assert!(Cache::at(PathBuf::from("/")).is_err());
    let tmp = tempfile::tempdir().unwrap();
    assert!(Cache::at(tmp.path().join("a/b")).is_ok());
}
