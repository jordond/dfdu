use std::sync::atomic::AtomicBool;
use std::time::Duration;

use dfdu_tree::{Ix, Kind, Meta, Tree};

use super::*;

/// A tree of the directory `root` with `files` files in it.
pub(super) fn tree_of(root: &Path, files: usize) -> (Tree, Ix) {
    let mut t = Tree::new();
    let meta = Meta::from_metadata(&fs::metadata(root).unwrap());
    let r = t.new_root(root.as_os_str().as_encoded_bytes(), &meta, true);
    if let Some(d) = t.dir_mut(r) {
        d.state = dfdu_tree::DirState::Complete;
    }
    for i in 0..files {
        let m = Meta {
            kind: Kind::File,
            apparent: i as u64,
            alloc: 4096,
            ino: 1000 + i as u64,
            nlink: 1,
            ..meta
        };
        let f = t.add_file(r, format!("file-{i}").as_bytes(), &m, true, None);
        let own = t.own(f);
        t.propagate_add(r, &own);
    }
    (t, r)
}

pub(super) fn header(written: u64, last_full_scan: u64) -> Header {
    Header {
        tool_version: "test".into(),
        written,
        cross_mounts: false,
        network: false,
        journal: None,
        last_full_scan,
        scan_ms: 4_000,
    }
}

/// A cache in a temporary directory, a directory to scan, and its slot.
pub(super) fn setup() -> (tempfile::TempDir, Cache, Slot) {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("scanned")).unwrap();
    let cache = Cache::at(tmp.path().join("cache/scans")).unwrap();
    let key = Key::new(&tmp.path().join("scanned"), false, false, false).unwrap();
    let slot = cache.slot(key);
    (tmp, cache, slot)
}

pub(super) fn save(slot: &Slot, t: &Tree, h: &Header) -> Saved {
    slot.save(h, |f| f(t), &AtomicBool::new(false)).unwrap()
}

pub(super) fn set_mtime(path: &Path, secs: u64) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

#[test]
fn keys_follow_the_directory_and_the_options() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    fs::create_dir(&dir).unwrap();
    let k = Key::new(&dir, false, false, false).unwrap();
    // The same directory by another name is the same key.
    assert_eq!(Key::new(&dir.join("."), false, false, false).unwrap(), k);
    assert_eq!(k.root(), dunce::canonicalize(&dir).unwrap());
    let name = k.file_name();
    assert_eq!(name.len(), 16 + ".snap".len(), "{name}");
    assert_eq!(Path::new(&name).extension(), Some("snap".as_ref()));
    let names: std::collections::HashSet<String> = [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ]
    .iter()
    .map(|&(c, n, m)| Key::new(&dir, c, n, m).unwrap().file_name())
    .collect();
    assert_eq!(names.len(), 4);
    // A directory replaced by another of the same name is another key.
    fs::remove_dir(&dir).unwrap();
    fs::create_dir(tmp.path().join("other")).unwrap();
    fs::create_dir(&dir).unwrap();
    let again = Key::new(&dir, false, false, false).unwrap();
    assert_eq!(again.root(), k.root());
    assert_ne!(again.file_name(), name);
    fs::write(tmp.path().join("f"), b"").unwrap();
    assert!(Key::new(&tmp.path().join("f"), false, false, false).is_err());
    assert!(Key::new(&tmp.path().join("missing"), false, false, false).is_err());
}

/// The sweep removes expired files, then the least recently written ones over the cap, but
/// never the one to keep, and leaves other files alone.
#[test]
fn the_sweep_keeps_the_newest_under_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::at(tmp.path().to_path_buf()).unwrap();
    let now = now();
    let file = |name: &str, len: usize, age: u64| {
        let p = tmp.path().join(name);
        fs::write(&p, vec![0u8; len]).unwrap();
        set_mtime(&p, now - age);
        p
    };
    let expired = file("000000000000000a.snap", 10, MAX_AGE + 1);
    let newest = file("000000000000000b.snap", 100, 10);
    let middle = file("000000000000000c.snap", 100, 20);
    let oldest = file("000000000000000d.snap", 100, 30);
    let other = file("notes.txt", 1000, MAX_AGE + 1);
    // Not named as the cache names its files.
    let users = [
        file("notes.snap", 1000, MAX_AGE + 1),
        file("000000000000000E.snap", 1000, MAX_AGE + 1),
        file(".notes.snap.1.tmp", 10, 0),
    ];
    let dead = file(".000000000000000b.snap.1.tmp", 10, 0);
    assert_eq!(
        cache.sweep(now, 250, Some(&oldest)),
        Removed {
            files: 2,
            bytes: 110
        }
    );
    assert!(!expired.exists() && !middle.exists() && !dead.exists());
    assert!(newest.exists() && oldest.exists() && other.exists());
    assert!(users.iter().all(|p| p.exists()));
    assert_eq!(cache.sweep(now, 250, None).files, 0);
    assert_eq!(cache.sweep(now, 150, None).files, 1);
    assert!(newest.exists() && !oldest.exists());
    assert_eq!(cache.clear().unwrap().files, 1);
    assert!(!newest.exists() && other.exists());
    assert!(users.iter().all(|p| p.exists()));
}

#[test]
fn only_the_caches_own_names_are_its_files() {
    assert!(is_scan_name("0123456789abcdef.snap"));
    for n in [
        "0123456789abcde.snap",
        "0123456789ABCDEF.snap",
        "0123456789abcdefa.snap",
        "0123456789abcdef.snapx",
        "0123456789abcdefsnap",
        "notes.snap",
        ".0123456789abcdef.snap",
    ] {
        assert!(!is_scan_name(n), "{n}");
    }
    assert!(is_temp_name(".0123456789abcdef.snap.42.tmp"));
    for n in [
        ".0123456789abcdef.snap..tmp",
        ".0123456789abcdef.snap.4x.tmp",
        ".notes.snap.1.tmp",
        "0123456789abcdef.snap.1.tmp",
        ".0123456789abcdef.snap.1",
    ] {
        assert!(!is_temp_name(n), "{n}");
    }
}

/// The directory created is the user's alone and the files are readable by the user only. A
/// directory that exists is used as it is, unless other users can write to it.
#[cfg(unix)]
#[test]
fn files_are_private() {
    use std::os::unix::fs::PermissionsExt;
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
#[cfg(unix)]
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

/// The listing reads each file's root, header and entries.
#[test]
fn the_list_says_what_each_file_holds() {
    let (_tmp, cache, slot) = setup();
    let now = now();
    let (t, _) = tree_of(slot.key.root(), 4);
    save(&slot, &t, &header(now, now));
    fs::write(cache.dir().join("0123456789abcdef.snap"), b"DFDUSNAP").unwrap();
    let list = cache.list();
    assert_eq!(list.len(), 2);
    let ok = list.iter().find(|l| l.path == slot.path).unwrap();
    let (root, h) = ok.scan.as_ref().unwrap();
    assert_eq!(root, slot.key.root());
    assert_eq!(h.scan_ms, 4_000);
    assert_eq!(ok.entries, Some(5));
    assert!(list.iter().any(|l| l.scan.is_none()));
}

#[test]
fn the_cache_turns_off_by_environment() {
    for v in ["0", "false", "OFF", "no", " 0 "] {
        assert!(is_off(std::ffi::OsStr::new(v)), "{v}");
    }
    for v in ["1", "", "yes", "on"] {
        assert!(!is_off(std::ffi::OsStr::new(v)), "{v}");
    }
}
