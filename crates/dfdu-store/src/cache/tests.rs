use std::sync::Mutex;

use dfdu_tree::{Ix, Kind, Meta};

use super::*;
use crate::ncdu::tests::dump;

/// A tree of the directory `root` with `files` files in it.
fn tree_of(root: &Path, files: usize) -> (Tree, Ix) {
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

fn header(written: u64, last_full_scan: u64) -> Header {
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
fn setup() -> (tempfile::TempDir, Cache, Slot) {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("scanned")).unwrap();
    let cache = Cache::at(tmp.path().join("cache/scans")).unwrap();
    let key = Key::new(&tmp.path().join("scanned"), false, false, false).unwrap();
    let slot = cache.slot(key);
    (tmp, cache, slot)
}

fn save(slot: &Slot, t: &Tree, h: &Header) -> Saved {
    slot.save(h, |f| f(t), &AtomicBool::new(false)).unwrap()
}

fn set_mtime(path: &Path, secs: u64) {
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
    assert!(name.ends_with(".snap"));
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

/// What is saved loads back as it was: the tree, and the header.
#[test]
fn a_saved_scan_loads_back() {
    let (_tmp, _cache, slot) = setup();
    let now = now();
    assert!(!slot.cached(now));
    assert!(slot.load(now).unwrap().is_none());
    let (t, r) = tree_of(slot.key.root(), 50);
    let h = header(now, now - 60);
    let Saved::Written { entries, bytes, .. } = save(&slot, &t, &h) else {
        panic!("not written")
    };
    assert_eq!(entries, 51);
    assert_eq!(bytes, fs::metadata(&slot.path).unwrap().len());
    assert!(slot.cached(now));
    let hit = slot.load(now).unwrap().expect("a hit");
    assert!(!hit.full_rescan);
    assert_eq!(hit.loaded.header, h);
    assert_eq!(dump(&t, r), dump(&hit.loaded.tree, hit.loaded.root));
}

/// A file written more than the maximum age ago is not loaded and is removed; a tree whose
/// last full scan is older than a week is loaded to be rescanned whole.
#[test]
fn old_scans_expire() {
    let (_tmp, _cache, slot) = setup();
    let now = now();
    let (t, _) = tree_of(slot.key.root(), 3);
    save(&slot, &t, &header(now - FULL_RESCAN_AFTER - 10, now - FULL_RESCAN_AFTER - 10));
    let hit = slot.load(now).unwrap().expect("a hit");
    assert!(hit.full_rescan);
    // Brought up to date from the journal since: still the old full scan.
    save(&slot, &t, &header(now, now - FULL_RESCAN_AFTER - 10));
    assert!(slot.load(now).unwrap().expect("a hit").full_rescan);
    save(&slot, &t, &header(now, now - FULL_RESCAN_AFTER + 10));
    assert!(!slot.load(now).unwrap().expect("a hit").full_rescan);
    // Formats before 4 do not say; when it was written stands in.
    save(&slot, &t, &header(now - 60, 0));
    assert!(!slot.load(now).unwrap().expect("a hit").full_rescan);

    save(&slot, &t, &header(now - MAX_AGE - 10, now - MAX_AGE - 10));
    assert!(slot.cached(now), "the file itself is new");
    assert!(slot.load(now).unwrap().is_none());
    assert!(!slot.path.exists(), "an expired scan is removed");
    save(&slot, &t, &header(now, now));
    set_mtime(&slot.path, now - MAX_AGE - 10);
    assert!(!slot.cached(now));
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
    let expired = file("a.snap", 10, MAX_AGE + 1);
    let newest = file("b.snap", 100, 10);
    let middle = file("c.snap", 100, 20);
    let oldest = file("d.snap", 100, 30);
    let other = file("notes.txt", 1000, MAX_AGE + 1);
    assert_eq!(
        cache.sweep(now, 250, Some(&oldest)),
        Removed {
            files: 2,
            bytes: 110
        }
    );
    assert!(!expired.exists() && !middle.exists());
    assert!(newest.exists() && oldest.exists() && other.exists());
    assert_eq!(cache.sweep(now, 250, None).files, 0);
    assert_eq!(cache.sweep(now, 150, None).files, 1);
    assert!(newest.exists() && !oldest.exists());
    assert_eq!(cache.clear().unwrap().files, 1);
    assert!(!newest.exists() && other.exists());
}

/// A damaged or cut-short file is an error and is removed; one of another directory is not
/// used, nor is a symbolic link.
#[test]
fn bad_files_are_not_loaded() {
    let (tmp, _cache, slot) = setup();
    let now = now();
    let (t, _) = tree_of(slot.key.root(), 20);
    save(&slot, &t, &header(now, now));
    let good = fs::read(&slot.path).unwrap();
    fs::write(&slot.path, &good[..good.len() / 2]).unwrap();
    assert!(slot.load(now).is_err());
    assert!(!slot.path.exists());
    fs::write(&slot.path, b"not a snapshot").unwrap();
    assert!(slot.load(now).is_err());
    assert!(!slot.path.exists());

    // Another directory's scan under this name.
    fs::create_dir(tmp.path().join("elsewhere")).unwrap();
    let (other, _) = tree_of(&dunce::canonicalize(tmp.path().join("elsewhere")).unwrap(), 2);
    let mut bytes = vec![];
    snapshot::write(&other, other.roots().next().unwrap(), &mut bytes, &header(now, now)).unwrap();
    fs::write(&slot.path, &bytes).unwrap();
    assert!(slot.load(now).unwrap().is_none());
    // Other options.
    let mut cross = header(now, now);
    cross.cross_mounts = true;
    let mut bytes = vec![];
    snapshot::write(&t, t.roots().next().unwrap(), &mut bytes, &cross).unwrap();
    fs::write(&slot.path, &bytes).unwrap();
    assert!(slot.load(now).unwrap().is_none());

    #[cfg(unix)]
    {
        let real = tmp.path().join("real.snap");
        fs::write(&real, &good).unwrap();
        fs::remove_file(&slot.path).unwrap();
        std::os::unix::fs::symlink(&real, &slot.path).unwrap();
        assert!(!slot.cached(now));
        assert!(slot.load(now).unwrap().is_none());
        assert!(real.exists());
    }
}

/// The directory is the user's alone and the files are readable by the user only; a looser
/// directory is tightened.
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
    save(&slot, &t, &header(now(), now()));
    assert_eq!(mode(cache.dir()), 0o700);
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
    assert!(Cache::at(PathBuf::from("/dfdu-test-no-such-dir/scans")).is_none());
    let tmp = tempfile::tempdir().unwrap();
    assert!(Cache::at(tmp.path().join("a/b")).is_some());
}

/// A tree that changes between the parts of a save is not written, nor is anything once
/// cancelled; the file cached before stays, and no temporary file is left.
#[test]
fn a_changing_tree_or_a_cancel_writes_nothing() {
    let (_tmp, cache, slot) = setup();
    let now = now();
    let (t, _) = tree_of(slot.key.root(), STEP_ENTRIES + 10);
    let h = header(now, now);
    assert!(matches!(save(&slot, &t, &h), Saved::Written { .. }));
    let before = fs::read(&slot.path).unwrap();
    let tree = Mutex::new(t);
    let mut calls = 0;
    let changed = slot
        .save(
            &header(now + 1, now + 1),
            |f| {
                let mut t = tree.lock().unwrap();
                if calls == 1 {
                    t.bump_version();
                }
                calls += 1;
                f(&t);
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(changed, Saved::Changed);
    assert_eq!(calls, 2);
    let t = tree.into_inner().unwrap();
    let cancelled = slot.save(&h, |f| f(&t), &AtomicBool::new(true)).unwrap();
    assert_eq!(cancelled, Saved::Cancelled);
    // A tree of another directory.
    let (other, _) = tree_of(cache.dir(), 1);
    assert_eq!(save(&slot, &other, &h), Saved::Changed);
    assert_eq!(fs::read(&slot.path).unwrap(), before);
    assert_eq!(fs::read_dir(cache.dir()).unwrap().count(), 1);
}

/// The listing reads each file's root, header and entries.
#[test]
fn the_list_says_what_each_file_holds() {
    let (_tmp, cache, slot) = setup();
    let now = now();
    let (t, _) = tree_of(slot.key.root(), 4);
    save(&slot, &t, &header(now, now));
    fs::write(cache.dir().join("broken.snap"), b"DFDUSNAP").unwrap();
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
