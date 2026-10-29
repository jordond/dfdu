//! The fixture mounted: what `stat`, `read`, `rm -r`, `rename` and `statvfs` see through the
//! kernel matches the tree.
//!
//! Needs `/dev/fuse` and root (or `fusermount3`); skipped with a message otherwise, unless
//! `DFDU_FUSE_REQUIRED` is set (CI).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_demofs::Fixture;

const FIXTURE: &str = r#"
now = "2026-10-01T09:00:00Z"

[disk]
total = "100GB"
free = "10GB"
device = "/dev/nvme0n1p2"

[[entry]]
path = "Downloads/a.iso"
size = "3MiB"
content = "iso"
age = "200d"

[[entry]]
path = "Downloads/old/a (1).iso"
size = "3MiB"
content = "iso"

[[entry]]
path = "dev/shop/package.json"
text = "{}"

[[entry]]
path = "dev/shop/node_modules"
fill = { files = 300, size = "40MiB", style = "npm" }

[[entry]]
path = "dev/shop/hard"
link = "dev/shop/package.json"

[[entry]]
path = "dev/shop/current"
target = "package.json"

[[entry]]
path = "Library/locked"
kind = "dir"
error = "denied"

[[entry]]
path = "Library/broken.db"
size = 100
error = "io"

[[entry]]
path = "many"
fill = { files = 2000, size = "2MiB" }

[[entry]]
path = "slow"
kind = "dir"
delay = "300ms"
"#;

fn mounted(at: &Path) -> Option<fuser::BackgroundSession> {
    let f = Fixture::parse(FIXTURE).unwrap();
    match dfdu_demofs::mount(&f, at) {
        Ok(s) => Some(s),
        Err(e) if std::env::var_os("DFDU_FUSE_REQUIRED").is_none() => {
            eprintln!("skipped: cannot mount a FUSE filesystem ({e:#}); needs /dev/fuse and root");
            None
        }
        Err(e) => panic!("cannot mount a FUSE filesystem: {e:#}"),
    }
}

/// Bytes allocated below `p`, each inode once, as `du` counts them; what cannot be stat'ed
/// counts nothing.
fn du(p: &Path, seen: &mut std::collections::HashSet<u64>) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else {
        return 0;
    };
    let mut total = if seen.insert(m.ino()) {
        m.blocks() * 512
    } else {
        0
    };
    if m.is_dir()
        && let Ok(rd) = fs::read_dir(p)
    {
        for e in rd {
            total += du(&e.unwrap().path(), seen);
        }
    }
    total
}

fn free(p: &Path) -> u64 {
    let s = rustix::fs::statvfs(p).unwrap();
    s.f_bavail * s.f_frsize
}

#[test]
fn serves_and_changes_the_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    fs::create_dir(&root).unwrap();
    let Some(session) = mounted(&root) else {
        return;
    };
    let tree = Fixture::parse(FIXTURE).unwrap().build().unwrap();

    // The mount table names it as a local FUSE filesystem with the fixture's device.
    let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let line = mounts
        .lines()
        .find(|l| l.contains(root.to_str().unwrap()))
        .unwrap();
    assert!(
        line.contains(" - fuse.dfdu-demofs /dev/nvme0n1p2 "),
        "{line}"
    );

    // Sizes, blocks and times as the fixture says; du of the root is the tree's use, less the
    // denied directory's contents (it has none) and the file whose stat fails.
    let iso = fs::metadata(root.join("Downloads/a.iso")).unwrap();
    assert_eq!((iso.len(), iso.blocks() * 512), (3 << 20, 3 << 20));
    let now = Fixture::parse(FIXTURE).unwrap().now().unwrap();
    assert_eq!(iso.mtime(), now - 200 * 86_400);
    let broken = fs::metadata(root.join("Library/broken.db")).unwrap_err();
    assert_eq!(broken.raw_os_error(), Some(5)); // EIO
    assert_eq!(
        du(&root, &mut Default::default()),
        tree.used() - 4096 // broken.db's one block
    );

    // Duplicates read the same; links and symlinks resolve.
    assert_eq!(
        fs::read(root.join("Downloads/a.iso")).unwrap(),
        fs::read(root.join("Downloads/old/a (1).iso")).unwrap()
    );
    let pkg = fs::metadata(root.join("dev/shop/package.json")).unwrap();
    assert_eq!(pkg.nlink(), 2);
    assert_eq!(
        fs::metadata(root.join("dev/shop/hard")).unwrap().ino(),
        pkg.ino()
    );
    assert_eq!(
        fs::read_link(root.join("dev/shop/current")).unwrap(),
        Path::new("package.json")
    );

    // Faults and delays.
    let locked = fs::read_dir(root.join("Library/locked")).unwrap_err();
    assert_eq!(locked.kind(), ErrorKind::PermissionDenied);
    let t = Instant::now();
    fs::read_dir(root.join("slow")).unwrap().for_each(drop);
    assert!(t.elapsed() >= Duration::from_millis(300));

    // Deleting a large tree frees its space; renames and writes work as the Trash needs.
    let before = free(&root);
    fs::remove_dir_all(root.join("dev/shop/node_modules")).unwrap();
    assert!(!root.join("dev/shop/node_modules").exists());
    assert!(free(&root) >= before + (40 << 20));
    let trash = root.join(".local/share/Trash");
    fs::create_dir_all(trash.join("files")).unwrap();
    fs::create_dir_all(trash.join("info")).unwrap();
    fs::write(
        trash.join("info/a.iso.trashinfo"),
        "[Trash Info]\nPath=/x\n",
    )
    .unwrap();
    fs::rename(root.join("Downloads/a.iso"), trash.join("files/a.iso")).unwrap();
    assert_eq!(
        fs::read_to_string(trash.join("info/a.iso.trashinfo")).unwrap(),
        "[Trash Info]\nPath=/x\n"
    );
    // The fixture's entries are its owner's, what is made here the maker's (root).
    assert_eq!(pkg.uid(), 1000);
    let made = fs::metadata(trash.join("info/a.iso.trashinfo")).unwrap();
    assert_eq!((made.uid(), made.gid()), (0, 0));
    assert_eq!(
        fs::metadata(trash.join("files/a.iso")).unwrap().ino(),
        iso.ino()
    );
    let names: Vec<_> = fs::read_dir(root.join("Downloads"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["old"]);
    drop(session);
}

#[test]
fn removing_while_listing_neither_skips_nor_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    fs::create_dir(&root).unwrap();
    let Some(session) = mounted(&root) else {
        return;
    };
    let many = root.join("many");
    // Hard links in the same directory, which share an inode number.
    for k in 0..3 {
        fs::hard_link(many.join("file-0001.dat"), many.join(format!("link-{k}"))).unwrap();
    }
    // 2003 entries take many readdir calls; each is removed as soon as it is listed, so every
    // call after the first resumes in a directory that has changed since.
    let mut seen = std::collections::HashSet::new();
    for e in fs::read_dir(&many).unwrap() {
        let name = e.unwrap().file_name();
        assert!(seen.insert(name.clone()), "{name:?} listed twice");
        fs::remove_file(many.join(&name)).unwrap();
    }
    assert_eq!(seen.len(), 2003);
    assert_eq!(fs::read_dir(&many).unwrap().count(), 0);
    fs::remove_dir(&many).unwrap();
    drop(session);
}
