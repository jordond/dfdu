//! Changes made through the kernel: `rm -r`, `mkdir`, writes, `rename`, hard links, and the
//! free space `statvfs` reports after them.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use super::mounted;

/// Free space on the filesystem of `p`, as `df` shows it.
fn free(p: &Path) -> u64 {
    let s = rustix::fs::statvfs(p).unwrap();
    s.f_bavail * s.f_frsize
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
