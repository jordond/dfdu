use super::*;

#[test]
fn plain_directory_is_not_a_subvolume() {
    let d = tempfile::tempdir().unwrap();
    if is_btrfs(d.path()).unwrap() {
        // The temp dir itself is on btrfs; the btrfs tests cover this case.
        return;
    }
    assert_eq!(subvol_info(d.path()).unwrap(), None);
    let fd = open_dir(CWD, d.path()).unwrap();
    let e = subvol_info_fd(&fd).unwrap_err();
    assert_eq!(
        e.raw_os_error(),
        Some(rustix::io::Errno::NOTTY.raw_os_error())
    );
}

#[test]
fn mount_id_is_shared_within_a_filesystem() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let (a, b) = (
        mount_id(d.path()).unwrap(),
        mount_id(&d.path().join("sub")).unwrap(),
    );
    assert!(a.is_some(), "kernel reports mount ids");
    assert_eq!(a, b);
    // /proc is always its own mount.
    assert_ne!(mount_id(Path::new("/proc")).unwrap(), a);
}

#[test]
fn destroy_rejects_bad_names() {
    let d = tempfile::tempdir().unwrap();
    let dir = open_dir(CWD, d.path()).unwrap();
    for bad in ["", "a/b", "a\0b"] {
        let e = destroy_subvolume_at(&dir, OsStr::new(bad)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
}
