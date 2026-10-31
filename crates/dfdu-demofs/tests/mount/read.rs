//! What `stat`, `read`, `readlink` and listings see of the fixture through the kernel.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_demofs::Fixture;

use super::{FIXTURE, mounted};

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
