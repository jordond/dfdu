//! The fixture mounted: what `stat`, `read`, `rm -r`, `rename` and `statvfs` see through the
//! kernel matches the tree.
//!
//! Needs `/dev/fuse` and root (or `fusermount3`); skipped with a message otherwise, unless
//! `DFDU_FUSE_REQUIRED` is set (CI).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
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

/// The fixture mounted at `root`, in a directory of its own; unmounted and removed when
/// dropped, in that order.
struct Mounted {
    root: PathBuf,
    _session: fuser::BackgroundSession,
    _dir: tempfile::TempDir,
}

/// Mounts the fixture, or prints why not and returns `None` (no `/dev/fuse`, no privilege)
/// unless `DFDU_FUSE_REQUIRED` is set.
fn mounted() -> Option<Mounted> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    fs::create_dir(&root).unwrap();
    let f = Fixture::parse(FIXTURE).unwrap();
    match dfdu_demofs::mount(&f, &root) {
        Ok(session) => Some(Mounted {
            root,
            _session: session,
            _dir: dir,
        }),
        Err(e) if std::env::var_os("DFDU_FUSE_REQUIRED").is_none() => {
            eprintln!("skipped: cannot mount a FUSE filesystem ({e:#}); needs /dev/fuse and root");
            None
        }
        Err(e) => panic!("cannot mount a FUSE filesystem: {e:#}"),
    }
}

/// Bytes allocated below `p`, each inode once, as `du` counts them; what cannot be stat'ed
/// counts nothing.
fn du(p: &Path, seen: &mut HashSet<u64>) -> u64 {
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
fn the_mount_table_names_a_local_fuse_filesystem_on_the_fixtures_device() {
    let Some(m) = mounted() else {
        return;
    };
    let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let line = mounts
        .lines()
        .find(|l| l.contains(m.root.to_str().unwrap()))
        .unwrap();
    assert!(
        line.contains(" - fuse.dfdu-demofs /dev/nvme0n1p2 "),
        "{line}"
    );
}

#[test]
fn sizes_blocks_and_times_are_the_fixtures() {
    let Some(m) = mounted() else {
        return;
    };
    let fixture = Fixture::parse(FIXTURE).unwrap();
    let iso = fs::metadata(m.root.join("Downloads/a.iso")).unwrap();
    assert_eq!((iso.len(), iso.blocks() * 512), (3 << 20, 3 << 20));
    assert_eq!(iso.mtime(), fixture.now().unwrap() - 200 * 86_400);
    let broken = fs::metadata(m.root.join("Library/broken.db")).unwrap_err();
    assert_eq!(broken.raw_os_error(), Some(5)); // EIO
    // du of the root is the tree's use, less the denied directory's contents (it has none)
    // and the file whose stat fails.
    let tree = fixture.build().unwrap();
    assert_eq!(
        du(&m.root, &mut HashSet::new()),
        tree.used() - 4096 // broken.db's one block
    );
}

#[test]
fn duplicates_read_the_same_and_links_resolve() {
    let Some(m) = mounted() else {
        return;
    };
    assert_eq!(
        fs::read(m.root.join("Downloads/a.iso")).unwrap(),
        fs::read(m.root.join("Downloads/old/a (1).iso")).unwrap()
    );
    let pkg = fs::metadata(m.root.join("dev/shop/package.json")).unwrap();
    assert_eq!(pkg.nlink(), 2);
    assert_eq!(
        fs::metadata(m.root.join("dev/shop/hard")).unwrap().ino(),
        pkg.ino()
    );
    assert_eq!(
        fs::read_link(m.root.join("dev/shop/current")).unwrap(),
        Path::new("package.json")
    );
    // The fixture's entries are its owner's.
    assert_eq!((pkg.uid(), pkg.gid()), (1000, 1000));
}

#[test]
fn faults_and_delays_reach_the_caller() {
    let Some(m) = mounted() else {
        return;
    };
    let locked = fs::read_dir(m.root.join("Library/locked")).unwrap_err();
    assert_eq!(locked.kind(), ErrorKind::PermissionDenied);
    let t = Instant::now();
    fs::read_dir(m.root.join("slow")).unwrap().for_each(drop);
    assert!(t.elapsed() >= Duration::from_millis(300));
}

#[test]
fn deleting_frees_space_and_the_trash_can_be_used() {
    let Some(m) = mounted() else {
        return;
    };
    // Deleting a large tree frees its space.
    let before = free(&m.root);
    fs::remove_dir_all(m.root.join("dev/shop/node_modules")).unwrap();
    assert!(!m.root.join("dev/shop/node_modules").exists());
    assert!(free(&m.root) >= before + (40 << 20));
    // Directories, writes and renames work as the Trash needs.
    let iso = fs::metadata(m.root.join("Downloads/a.iso")).unwrap();
    let trash = m.root.join(".local/share/Trash");
    fs::create_dir_all(trash.join("files")).unwrap();
    fs::create_dir_all(trash.join("info")).unwrap();
    fs::write(
        trash.join("info/a.iso.trashinfo"),
        "[Trash Info]\nPath=/x\n",
    )
    .unwrap();
    fs::rename(m.root.join("Downloads/a.iso"), trash.join("files/a.iso")).unwrap();
    assert_eq!(
        fs::read_to_string(trash.join("info/a.iso.trashinfo")).unwrap(),
        "[Trash Info]\nPath=/x\n"
    );
    // What is made here is the maker's (root).
    let made = fs::metadata(trash.join("info/a.iso.trashinfo")).unwrap();
    assert_eq!((made.uid(), made.gid()), (0, 0));
    assert_eq!(
        fs::metadata(trash.join("files/a.iso")).unwrap().ino(),
        iso.ino()
    );
    let names: Vec<_> = fs::read_dir(m.root.join("Downloads"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["old"]);
}

#[test]
fn removing_while_listing_neither_skips_nor_repeats() {
    let Some(m) = mounted() else {
        return;
    };
    let many = m.root.join("many");
    // Hard links in the same directory, which share an inode number.
    for k in 0..3 {
        fs::hard_link(many.join("file-0001.dat"), many.join(format!("link-{k}"))).unwrap();
    }
    // 2003 entries take many readdir calls; each is removed as soon as it is listed, so every
    // call after the first resumes in a directory that has changed since.
    let mut seen = HashSet::new();
    for e in fs::read_dir(&many).unwrap() {
        let name = e.unwrap().file_name();
        assert!(seen.insert(name.clone()), "{name:?} listed twice");
        fs::remove_file(many.join(&name)).unwrap();
    }
    assert_eq!(seen.len(), 2003);
    assert_eq!(fs::read_dir(&many).unwrap().count(), 0);
    fs::remove_dir(&many).unwrap();
}
