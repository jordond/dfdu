use super::*;
use std::fs;

/// Every entry of a directory as `(name, meta)`, sorted by name, read `per_block` entries
/// at a time.
fn list<B: Backend<Loc = PathBuf>>(
    b: &B,
    dir: &Path,
    per_block: usize,
) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
    crate::backend::tests::list(b, dir, dfdu_sys::Storage::Unknown, per_block)
}

#[test]
fn entries_match_the_portable_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::backend::tests::populate(root);
    std::os::unix::fs::symlink("sub", root.join("sym")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
    // A sparse file, an empty file, a FIFO and a file with a resource fork (counted in
    // `st_blocks`, not in `st_size`).
    fs::File::create(root.join("sparse"))
        .unwrap()
        .set_len(10 << 20)
        .unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    let fifo = std::process::Command::new("mkfifo")
        .arg(root.join("fifo"))
        .status()
        .unwrap();
    assert!(fifo.success());
    fs::write(root.join("forked"), b"data").unwrap();
    // Where the filesystem has resource forks.
    let _ = fs::write(root.join("forked/..namedfork/rsrc"), vec![2u8; 50_000]);
    let ours = list(&MacBackend::default(), root, 3);
    let theirs = list(&PortableBackend::default(), root, 3);
    assert_eq!(ours.len(), 27);
    assert_eq!(ours, theirs);
    // A directory a buffer cannot hold, in one block and in many.
    let wide = root.join("sub");
    for i in 0..3000 {
        fs::write(wide.join(format!("{i:05}-{}", "w".repeat(100))), b"x").unwrap();
    }
    let theirs = list(&PortableBackend::default(), &wide, 4096);
    assert_eq!(theirs.len(), 3001);
    for per_block in [4096, 1000, 7] {
        assert_eq!(list(&MacBackend::default(), &wide, per_block), theirs);
    }
    let (m, p) = (MacBackend::default(), PortableBackend::default());
    let r = root.to_path_buf();
    assert_eq!(m.stat_root(&r).unwrap(), p.stat_root(&r).unwrap());
    // A symlink given as the root is followed.
    let s = m.loc_from_path(&root.join("sym"));
    assert_eq!(
        m.stat_root(&s).unwrap(),
        p.stat_root(&root.join("sym")).unwrap()
    );
    assert_eq!(list(&m, &s, 100).len(), 3001);
    // Below the root, a symlink is not opened as a directory.
    assert!(
        m.open(&root.join("sym"), dfdu_sys::Storage::Unknown)
            .is_err()
    );
}

/// The firmlinks at the top of the system volume (listed in `/usr/share/firmlinks`) are
/// links, so a scan of `/` does not read the Data volume through them.
#[test]
fn firmlinks_are_links() {
    let Ok(text) = fs::read_to_string("/usr/share/firmlinks") else {
        return;
    };
    let top: Vec<&[u8]> = text
        .lines()
        .filter_map(|l| l.split('\t').next()?.strip_prefix('/'))
        .filter(|p| !p.is_empty() && !p.contains('/'))
        .map(str::as_bytes)
        .collect();
    let mut seen = 0;
    for (name, meta) in list(&MacBackend::default(), Path::new("/"), 4096) {
        if top.contains(&&name[..]) {
            let m = meta.unwrap_or_else(|e| panic!("/{}: {e}", name.escape_ascii()));
            assert_eq!(m.kind, Kind::Symlink, "/{}: {m:?}", name.escape_ascii());
            assert_eq!((m.apparent, m.alloc), (0, 0));
            seen += 1;
        }
    }
    // `/Users` and `/Applications`, at least.
    assert!(seen >= 2, "{seen} firmlinks of {top:?}");
}

#[test]
fn unreadable_directory_is_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let e = MacBackend::default()
        .open(&locked, dfdu_sys::Storage::Unknown)
        .err()
        .unwrap();
    assert_eq!(ErrKind::from(&e), ErrKind::PermissionDenied);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}
