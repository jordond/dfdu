use super::*;

#[cfg(windows)]
mod windows;

/// The delete without the identity check, which only these tests use.
#[cfg(unix)]
fn remove(path: &Path, kind: Kind) -> io::Result<()> {
    imp::remove(path, kind, None)
}

#[cfg(unix)]
#[test]
fn removes_trees_without_following_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), b"k").unwrap();
    let d = tmp.path().join("d");
    std::fs::create_dir_all(d.join("a/b/c")).unwrap();
    std::fs::write(d.join("a/b/c/f"), b"x").unwrap();
    std::fs::write(d.join("top"), b"x").unwrap();
    std::os::unix::fs::symlink(outside.path(), d.join("a/link")).unwrap();
    remove(&d, Kind::Dir).unwrap();
    assert!(!d.exists());
    assert!(outside.path().join("keep").exists(), "followed a symlink");
    std::fs::write(tmp.path().join("f"), b"x").unwrap();
    remove(&tmp.path().join("f"), Kind::File).unwrap();
    assert!(!tmp.path().join("f").exists());
}

/// A directory above the item replaced by a symlink after the item was checked: the path
/// now names another directory, which is not deleted. (A symlink as the parent itself is
/// refused when the parent is opened.)
#[cfg(unix)]
#[test]
fn a_swapped_path_deletes_nothing() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let item = tmp.path().join("a/b/item");
    std::fs::create_dir_all(&item).unwrap();
    std::fs::write(tmp.path().join("a/b/file"), b"x").unwrap();
    let m = std::fs::symlink_metadata(&item).unwrap();
    let f = std::fs::symlink_metadata(tmp.path().join("a/b/file")).unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir_all(other.join("b/item")).unwrap();
    std::fs::write(other.join("b/item/precious"), b"p").unwrap();
    std::fs::write(other.join("b/file"), b"p").unwrap();
    std::fs::rename(tmp.path().join("a"), tmp.path().join("a-moved")).unwrap();
    std::os::unix::fs::symlink(&other, tmp.path().join("a")).unwrap();
    let e = remove_verified(&item, Kind::Dir, m.dev(), m.ino()).unwrap_err();
    assert!(e.to_string().contains("no longer the item"), "{e}");
    assert!(other.join("b/item/precious").exists());
    let file = tmp.path().join("a/b/file");
    let e = remove_verified(&file, Kind::File, f.dev(), f.ino()).unwrap_err();
    assert!(e.to_string().contains("no longer the item"), "{e}");
    assert!(other.join("b/file").exists());
    // Where they are now, they are deleted.
    let moved = tmp.path().join("a-moved/b");
    remove_verified(&moved.join("item"), Kind::Dir, m.dev(), m.ino()).unwrap();
    remove_verified(&moved.join("file"), Kind::File, f.dev(), f.ino()).unwrap();
    assert!(!moved.join("item").exists() && !moved.join("file").exists());
}

/// The directory renamed away and another put at its name while it is being emptied: the
/// final rmdir leaves the newcomer.
#[cfg(unix)]
#[test]
fn a_directory_swapped_in_while_emptying_is_not_removed() {
    use std::os::fd::AsFd;
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("d");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("f"), b"x").unwrap();
    let parent = imp::open_dir(rustix::fs::CWD, tmp.path().as_os_str()).unwrap();
    let dfd = imp::open_dir(&parent, std::ffi::OsStr::new("d")).unwrap();
    let (root, _) = imp::fs_of(dfd.as_fd()).unwrap();
    std::fs::rename(&d, tmp.path().join("moved")).unwrap();
    std::fs::create_dir(&d).unwrap();
    let e = imp::empty_and_remove(parent.as_fd(), std::ffi::OsStr::new("d"), dfd, root, &d)
        .unwrap_err();
    assert!(e.to_string().contains("no longer the item"), "{e}");
    assert!(d.is_dir(), "removed the directory swapped in");
    // The directory opened was emptied; it is where it was moved to.
    assert!(!tmp.path().join("moved/f").exists());
}

#[cfg(unix)]
#[test]
fn missing_item_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let e = remove(&tmp.path().join("nope"), Kind::Dir).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

/// Root only (mounts a tmpfs): a mount inside the item stops the delete there.
#[cfg(target_os = "linux")]
#[test]
fn stops_at_a_mount() {
    if !dfdu_sys::is_privileged() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("d");
    std::fs::create_dir_all(d.join("m")).unwrap();
    std::fs::write(d.join("before"), b"x").unwrap();
    let mounted = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "tmpfs"])
        .arg(d.join("m"))
        .status()
        .is_ok_and(|s| s.success());
    if !mounted {
        return;
    }
    std::fs::write(d.join("m/precious"), b"p").unwrap();
    let e = remove(&d, Kind::Dir).unwrap_err();
    let still = d.join("m/precious").exists();
    let _ = std::process::Command::new("umount")
        .arg(d.join("m"))
        .status();
    assert!(e.to_string().contains("mounted"), "{e}");
    assert!(still, "deleted into a mount");
}
