use super::*;
use std::fs;
use std::os::windows::ffi::OsStrExt;

/// Every entry of a directory as `(name, meta)`, sorted by name, read 3 entries at a time.
fn list<B: Backend>(b: &B, dir: &Path) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
    crate::backend::tests::list(b, dir, dfdu_sys::Storage::Unknown, 3)
}

/// A junction (no privilege needed, unlike symlinks); `false` if `mklink` failed.
fn junction(link: &Path, target: &Path) -> bool {
    std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn entries_match_the_portable_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    crate::backend::tests::populate(root);
    // Symlinks need Developer Mode or an elevated process (CI runners are elevated).
    let symlinks = std::os::windows::fs::symlink_dir("sub", root.join("sym")).is_ok()
        && std::os::windows::fs::symlink_file("nowhere", root.join("dangling")).is_ok();
    assert!(junction(&root.join("junction"), &root.join("sub")));
    // A name with an unpaired surrogate, which is not valid UTF-16.
    let bad: Vec<u16> = "bad-".encode_utf16().chain([0xD800]).collect();
    fs::write(root.join(OsString::from_wide(&bad)), b"x").unwrap();
    let ours = list(&WindowsBackend::default(), root);
    let theirs = list(&PortableBackend::default(), root);
    assert_eq!(ours.len(), if symlinks { 25 } else { 23 });
    assert_eq!(ours, theirs);
    let by_name = |n: &str| &ours.iter().find(|e| e.0 == n.as_bytes()).unwrap().1;
    assert_eq!(by_name("f3").unwrap().nlink, 2);
    assert_eq!(by_name("f4").unwrap().nlink, 1);
    assert_eq!(by_name("junction").unwrap().kind, Kind::Symlink);
    assert_eq!(by_name("sub").unwrap().kind, Kind::Dir);
    // File ids and the volume serial identify a file: the two names of f3 agree.
    let sub = list(&WindowsBackend::default(), &root.join("sub"));
    let (f3, link) = (by_name("f3").unwrap(), sub[0].1.unwrap());
    assert_eq!((f3.dev, f3.ino), (link.dev, link.ino));
    assert_ne!(f3.ino, by_name("f4").unwrap().ino);
    let (w, p) = (WindowsBackend::default(), PortableBackend::default());
    assert_eq!(
        w.stat_root(&w.loc_from_path(root)).unwrap(),
        p.stat_root(&root.to_path_buf()).unwrap()
    );
    // A junction given as the root is followed.
    let j = root.join("junction");
    let jm = w.stat_root(&w.loc_from_path(&j)).unwrap();
    assert_eq!(jm.kind, Kind::Dir);
    assert_eq!(list(&w, &j), sub);
}

#[test]
fn reads_paths_longer_than_max_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut deep = tmp.path().to_path_buf();
    // std adds the `\\?\` prefix to long paths itself.
    for i in 0..30 {
        deep.push(format!("{i:03}-{}", "y".repeat(200)));
        fs::create_dir(&deep).unwrap();
    }
    fs::write(deep.join("leaf"), [7u8; 99]).unwrap();
    // A name Win32 path parsing would strip the dot from.
    let dotted = dfdu_sys::verbatim_path(&deep).unwrap().join("dot.");
    fs::create_dir(&dotted).unwrap();
    fs::write(dotted.join("in"), [1u8; 5]).unwrap();
    assert!(deep.as_os_str().encode_wide().count() > 6000);
    let b = WindowsBackend::default();
    let entries = list(&b, &deep);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1].0, b"leaf");
    assert_eq!(entries[1].1.unwrap().apparent, 99);
    assert_eq!(entries[0].1.unwrap().kind, Kind::Dir);
    let mut s = b
        .open(&b.loc_from_path(&deep), dfdu_sys::Storage::Unknown)
        .unwrap();
    let mut block = Block::default();
    let (mut raw, _) = s.next_raw(10, 1 << 20).unwrap();
    raw.stat(&mut block).unwrap();
    let child = block
        .entries
        .iter()
        .find_map(|e| e.child.clone())
        .expect("dot. is a directory");
    let inner = b
        .open(&child, dfdu_sys::Storage::Unknown)
        .unwrap()
        .next_raw(10, 1 << 20)
        .unwrap()
        .0
        .len();
    assert_eq!(inner, 1);
    assert_eq!(
        b.stat_root(&b.loc_from_path(&deep)).unwrap().kind,
        Kind::Dir
    );
    drop(s);
    let _ = fs::remove_dir_all(&dotted);
}
