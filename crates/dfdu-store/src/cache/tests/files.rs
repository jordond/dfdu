//! The cache's files: which names are its own, the sweep, and the listing.

use super::*;

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
    assert!(is_temp_name(b".0123456789abcdef.snap.42.tmp"));
    for n in [
        ".0123456789abcdef.snap..tmp",
        ".0123456789abcdef.snap.4x.tmp",
        ".notes.snap.1.tmp",
        "0123456789abcdef.snap.1.tmp",
        ".0123456789abcdef.snap.1",
    ] {
        assert!(!is_temp_name(n.as_bytes()), "{n}");
    }
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
