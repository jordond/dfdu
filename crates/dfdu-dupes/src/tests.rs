use std::fs;
use std::path::Path;

use dfdu_tree::Meta;

use super::*;

fn cand(path: &Path) -> Candidate {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path).unwrap();
    Candidate {
        path: path.to_path_buf(),
        size: m.len(),
        alloc: m.blocks() * 512,
        dev_ino: (m.dev(), m.ino()),
        mtime: m.mtime(),
        links: 0,
        private: None,
    }
}

fn run(cands: &[Candidate]) -> Found {
    let opts = Options {
        min_size: 1,
        threads: 3,
    };
    find(cands, &opts, &Progress::default(), &AtomicBool::new(false))
}

fn names(g: &Group) -> Vec<String> {
    g.files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn identical_files_are_grouped_and_near_misses_are_not() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let big: Vec<u8> = (0..400_000u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(d.join("a"), &big).unwrap();
    fs::write(d.join("b"), &big).unwrap();
    fs::write(d.join("c"), &big).unwrap();
    // Same size, same first and last 64 KiB, one byte different in the middle: only the
    // full hash tells it apart.
    let mut middle = big.clone();
    middle[200_000] ^= 1;
    fs::write(d.join("m"), &middle).unwrap();
    // Same size, different start.
    let mut start = big.clone();
    start[0] ^= 1;
    fs::write(d.join("s"), &start).unwrap();
    // A small pair, hashed whole in the partial round.
    fs::write(d.join("x"), b"hello").unwrap();
    fs::write(d.join("y"), b"hello").unwrap();
    fs::write(d.join("z"), b"jello").unwrap();
    let all = ["a", "b", "c", "m", "s", "x", "y", "z"].map(|n| cand(&d.join(n)));
    let found = run(&all);
    assert!(!found.cancelled);
    assert_eq!(found.compared, 8);
    assert_eq!(found.skipped, 0);
    assert_eq!(found.groups.len(), 2, "{:#?}", found.groups);
    assert_eq!(names(&found.groups[0]), ["a", "b", "c"]);
    assert_eq!(found.groups[0].size, 400_000);
    assert_eq!(names(&found.groups[1]), ["x", "y"]);
    assert_eq!(found.groups[0].wasted(), 2 * all[0].alloc);
    let sha = Sha256::digest(&big);
    assert_eq!(found.groups[0].hash, <[u8; 32]>::from(sha));
}

/// The full hash is SHA-256 (the FIPS 180-2 test vector), as `dfdu dupes` reports it.
#[test]
fn full_hash_is_sha256() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("abc");
    fs::write(&f, b"abc").unwrap();
    let (hash, _) = full_hash(&cand(&f), &Progress::default(), &AtomicBool::new(false)).unwrap();
    assert_eq!(
        hex::encode(hash),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn changed_and_unreadable_files_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    fs::write(d.join("a"), [1u8; 5000]).unwrap();
    fs::write(d.join("b"), [1u8; 5000]).unwrap();
    fs::write(d.join("c"), [1u8; 5000]).unwrap();
    let cands = ["a", "b", "c"].map(|n| cand(&d.join(n)));
    // `c` grew, and `b` became a symlink to `a`, which is not followed.
    fs::write(d.join("c"), [1u8; 6000]).unwrap();
    fs::remove_file(d.join("b")).unwrap();
    std::os::unix::fs::symlink(d.join("a"), d.join("b")).unwrap();
    let found = run(&cands);
    assert!(found.groups.is_empty(), "{:#?}", found.groups);
    assert_eq!(found.skipped, 2);
    assert!(still_same(&cands[0]) && !still_same(&cands[1]) && !still_same(&cands[2]));
}

#[test]
fn a_fifo_in_place_of_a_file_does_not_block() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    fs::write(d.join("a"), [1u8; 5000]).unwrap();
    fs::write(d.join("b"), [1u8; 5000]).unwrap();
    let cands = ["a", "b"].map(|n| cand(&d.join(n)));
    fs::remove_file(d.join("b")).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(d.join("b"))
        .status()
        .unwrap();
    assert!(made.success());
    let found = run(&cands);
    assert!(found.groups.is_empty());
    assert_eq!(found.skipped, 1);
}

#[test]
fn cancel_returns_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a"), [1u8; 5000]).unwrap();
    fs::write(tmp.path().join("b"), [1u8; 5000]).unwrap();
    let cands = ["a", "b"].map(|n| cand(&tmp.path().join(n)));
    let found = find(
        &cands,
        &Options::default(),
        &Progress::default(),
        &AtomicBool::new(true),
    );
    assert!(found.cancelled && found.groups.is_empty());
}

#[test]
fn candidates_skip_small_files_links_placeholders_and_unique_sizes() {
    let meta = |kind, apparent, alloc, ino| Meta {
        kind,
        apparent,
        alloc,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    };
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 0, 1), true);
    let sub = t.add_dir(root, b"sub", &meta(Kind::Dir, 0, 0, 2));
    t.add_file(root, b"a", &meta(Kind::File, 1000, 1024, 10), false, None);
    t.add_file(sub, b"b", &meta(Kind::File, 1000, 1024, 11), false, None);
    // A second name of `a`'s inode.
    t.add_file(sub, b"a-link", &meta(Kind::File, 1000, 0, 10), false, None);
    // A placeholder of the same size: nothing allocated.
    t.add_file(root, b"cloud", &meta(Kind::File, 1000, 0, 12), false, None);
    // A size no other file has, and a small pair.
    t.add_file(
        root,
        b"lone",
        &meta(Kind::File, 2000, 2048, 13),
        false,
        None,
    );
    t.add_file(root, b"s1", &meta(Kind::File, 10, 512, 14), false, None);
    t.add_file(root, b"s2", &meta(Kind::File, 10, 512, 15), false, None);
    let mut c = candidates(&t, root, 100);
    c.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<_> = c
        .iter()
        .map(|c| c.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(paths, ["/r/a", "/r/sub/b"]);
    assert_eq!(c[0].links, 1);
    assert_eq!(candidates(&t, root, 1).len(), 4);
    // Counting finds the same files without building their paths.
    for min in [1, 100, 5000] {
        let n = candidates(&t, root, min).len();
        assert_eq!(count_candidates(&t, root, min), n, "{min}");
    }
}

/// APFS clones are identical but deleting one frees nothing; a real copy next to them does.
#[cfg(target_os = "macos")]
#[test]
fn clones_free_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    fs::write(d.join("a"), vec![3u8; 300_000]).unwrap();
    let ok = std::process::Command::new("cp")
        .arg("-c")
        .args([d.join("a"), d.join("b")])
        .status()
        .unwrap();
    assert!(ok.success());
    let found = run(&["a", "b"].map(|n| cand(&d.join(n))));
    let g = &found.groups[0];
    if g.files[0].private.is_none() {
        return; // not APFS
    }
    assert!(g.all_clones());
    assert_eq!(g.wasted(), 0);
    fs::write(d.join("c"), vec![3u8; 300_000]).unwrap();
    let found = run(&["a", "b", "c"].map(|n| cand(&d.join(n))));
    let g = &found.groups[0];
    assert_eq!(g.files.len(), 3);
    assert!(!g.all_clones());
    assert_eq!(g.wasted(), g.files[2].alloc);
}

/// Reflinked copies on btrfs share their extents: deleting one frees nothing. Runs on the
/// fixture of `scripts/btrfs-fixture.sh` (`DFDU_BTRFS_DIR`), skipped without it.
#[cfg(target_os = "linux")]
#[test]
fn reflinks_free_nothing() {
    let Some(f) = std::env::var_os("DFDU_BTRFS_DIR") else {
        return;
    };
    let tmp = tempfile::tempdir_in(f).unwrap();
    let d = tmp.path();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(d.join("a"), &data).unwrap();
    let ok = std::process::Command::new("cp")
        .arg("--reflink=always")
        .args([d.join("a"), d.join("b")])
        .status()
        .unwrap();
    assert!(ok.success());
    fs::write(d.join("c"), &data).unwrap();
    std::process::Command::new("sync").status().unwrap();
    let found = run(&["a", "b", "c"].map(|n| cand(&d.join(n))));
    let g = &found.groups[0];
    assert_eq!(g.files.len(), 3);
    let by = |n: &str| g.files.iter().find(|f| f.path.ends_with(n)).unwrap();
    assert_eq!(by("a").private, Some(0));
    assert_eq!(by("b").private, Some(0));
    assert_eq!(by("c").private, None);
    assert!(!g.all_clones());
}

#[test]
fn one_thread_reads_a_spinning_disk() {
    use dfdu_sys::Storage;
    assert_eq!(Options::for_storage(Storage::Rotational).threads, 1);
    for s in [Storage::Solid, Storage::Network, Storage::Unknown] {
        assert_eq!(Options::for_storage(s).threads, Options::default().threads);
    }
    let min = Options::default().min_size;
    assert_eq!(Options::for_storage(Storage::Rotational).min_size, min);
}
