//! Resuming a tree loaded from a file with the directories a change journal names: each is
//! listed again, the loaded subdirectories it still has are kept, and what is new is scanned.
//! The journal's answers are given by hand; the trees are on disk.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::{DirState, Ix, Meta, NodeRef, Totals, Tree};

mod common;
use common::tempdir;

fn engine() -> Engine<PortableBackend> {
    Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            check_invariants: true,
            ..Default::default()
        },
    )
}

fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; bytes]).unwrap();
}

/// The tree of `root` as a snapshot of it now would load: every directory complete, a hard
/// link charged where it is first met.
fn loaded(root: &Path) -> Tree {
    fn add(t: &mut Tree, dir: Ix, path: &Path, seen: &mut Vec<(u64, u64)>) {
        let mut entries: Vec<_> = fs::read_dir(path).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for e in entries {
            let file_name = e.file_name();
            let name = dfdu_tree::name_bytes(&file_name);
            let meta = Meta::from_metadata(&fs::symlink_metadata(e.path()).unwrap());
            if e.file_type().unwrap().is_dir() {
                let d = t.add_dir(dir, &name, &meta);
                let own = t.own(d);
                t.propagate_add(dir, &own);
                add(t, d, &e.path(), seen);
                close(t, d);
            } else {
                let key = (meta.dev, meta.ino);
                let charged = !meta.is_hardlink() || !seen.contains(&key);
                seen.push(key);
                let f = t.add_file(dir, &name, &meta, charged, None);
                let own = t.own(f);
                t.propagate_add(dir, &own);
            }
        }
    }
    fn close(t: &mut Tree, d: Ix) {
        let r = t.dir_mut(d).unwrap();
        r.state = DirState::Complete;
        r.outcome = r.state;
        r.pending = 0;
        r.listing_closed = true;
        r.reported = true;
    }
    let mut t = Tree::default();
    let meta = Meta::from_metadata(&fs::metadata(root).unwrap());
    let r = t.new_root(&dfdu_tree::name_bytes(root.as_os_str()), &meta, true);
    add(&mut t, r, root, &mut Vec::new());
    close(&mut t, r);
    t
}

/// The node at `rel` below the root, if there is one.
fn find(t: &Tree, rel: &str) -> Option<Ix> {
    let mut at = t.roots().next().unwrap();
    for n in rel.split('/').filter(|n| !n.is_empty()) {
        at = t.children(at).find(|&c| t.name(c) == n.as_bytes())?;
    }
    Some(at)
}

fn sizes(t: Totals) -> (u64, u64, u64) {
    (t.apparent, t.alloc, t.items)
}

/// The totals of a fresh scan of `root`.
fn scanned(root: &Path) -> (u64, u64, u64) {
    let e = engine();
    e.send(Command::Scan {
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let t = e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())));
    e.shutdown(Duration::from_secs(1));
    t
}

/// Loads `tree` and resumes it with `changed`, waiting until it is done.
fn resume(tree: Tree, changed: &[&str]) -> Engine<PortableBackend> {
    let e = engine();
    let node = tree.node_ref(tree.roots().next().unwrap());
    e.load(tree);
    e.send(Command::Resume {
        node,
        opts: JobOptions::default(),
        changed: Some(changed.iter().map(PathBuf::from).collect()),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    e
}

/// A change in the root lists the root again: what was removed goes, what is new is scanned,
/// the subdirectories still there keep their loaded subtrees (the very nodes), and the totals
/// are a scan's, hard links counted once. A renamed directory is scanned again.
#[test]
fn a_changed_root_keeps_its_unchanged_subdirectories() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("top"), 1_000);
    write(&root.join("keep/k1"), 5_000);
    write(&root.join("keep/sub/k2"), 7_000);
    write(&root.join("gone/g"), 2_000);
    write(&root.join("old/m"), 4_000);
    write(&root.join("deep/a/b/f"), 3_000);
    fs::hard_link(root.join("keep/k1"), root.join("link")).unwrap();
    let tree = loaded(&root);
    let kept: Vec<NodeRef> = ["keep", "keep/sub", "keep/sub/k2", "keep/k1"]
        .iter()
        .map(|p| tree.node_ref(find(&tree, p).unwrap()))
        .collect();

    write(&root.join("added"), 9_000);
    fs::remove_dir_all(root.join("gone")).unwrap();
    write(&root.join("new/deeper/n"), 11_000);
    fs::rename(root.join("old"), root.join("renamed")).unwrap();
    write(&root.join("renamed/late"), 6_000);
    write(&root.join("deep/a/b/more"), 8_000);
    fs::remove_file(root.join("link")).unwrap();
    fs::hard_link(root.join("keep/k1"), root.join("deep/a/link")).unwrap();

    let e = resume(tree, &["", "deep/a/b", "deep/a", "renamed", "new/deeper"]);
    e.read(|m| {
        let t = &m.tree;
        for p in ["added", "new/deeper/n", "renamed/m", "renamed/late", "deep/a/b/more"] {
            assert!(find(t, p).is_some(), "{p} is scanned");
        }
        for p in ["gone", "old", "link"] {
            assert!(find(t, p).is_none(), "{p} is gone");
        }
        for (r, p) in kept.iter().zip(["keep", "keep/sub", "keep/sub/k2", "keep/k1"]) {
            assert_eq!(t.resolve_visible(*r), find(t, p), "{p} is kept as loaded");
        }
    });
    let got = e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())));
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// What the journal does not name is not read: a file added there stays unseen, also in a
/// directory above a named one, which a rescan of the named one does not list.
#[test]
fn what_the_journal_does_not_name_stays_as_loaded() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("quiet/q"), 3_000);
    write(&root.join("a/b/c/f"), 1_000);
    write(&root.join("a/other/o"), 2_000);
    let tree = loaded(&root);
    let other = tree.node_ref(find(&tree, "a/other").unwrap());

    write(&root.join("quiet/unseen"), 40_000);
    write(&root.join("a/other/unseen"), 40_000);
    write(&root.join("a/b/c/new"), 5_000);
    write(&root.join("a/top"), 5_000);

    let e = resume(tree, &["a/b/c"]);
    e.read(|m| {
        let t = &m.tree;
        assert!(find(t, "a/b/c/new").is_some(), "a named directory is listed");
        assert!(find(t, "a/top").is_none(), "the directory above it is not");
        assert!(find(t, "quiet/unseen").is_none());
        assert!(find(t, "a/other/unseen").is_none());
        assert_eq!(t.resolve_visible(other), find(t, "a/other"));
    });
    e.shutdown(Duration::from_secs(1));
}

/// A tree loaded from a file refuses deletes and native commands until a resume has taken it
/// over: its sizes may be old anywhere below. Then the delete goes ahead.
#[cfg(unix)]
#[test]
fn a_loaded_tree_is_not_deleted_from_until_resumed() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("junk/big"), 2_000);
    write(&root.join("file"), 1_000);
    let tree = loaded(&root);
    let node = tree.node_ref(tree.roots().next().unwrap());
    let junk = tree.node_ref(find(&tree, "junk").unwrap());
    let file = tree.node_ref(find(&tree, "file").unwrap());
    let journal = tempdir();
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            journal_dir: Some(journal.path().to_path_buf()),
            workers: 1,
            check_invariants: true,
            ..Default::default()
        },
    );
    e.load(tree);
    e.send(Command::Delete {
        items: vec![junk, file],
        mode: Mode::Permanent,
    });
    e.send(Command::RunNative {
        id: 1,
        node: junk,
        variant: None,
        dry_run: false,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    e.read(|m| {
        let refused: Vec<_> = m
            .status
            .notices
            .iter()
            .filter(|n| n.contains("not refreshed yet"))
            .collect();
        assert_eq!(refused.len(), 3, "{:?}", m.status.notices);
        assert!(refused[0].starts_with("delete: junk "), "{refused:?}");
        assert!(refused[2].starts_with("native command: "), "{refused:?}");
    });
    assert!(root.join("junk/big").exists() && root.join("file").exists());

    e.send(Command::Resume {
        node,
        opts: JobOptions::default(),
        changed: Some(vec![]),
    });
    e.send(Command::Delete {
        items: vec![junk],
        mode: Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    let s = e.read(|m| m.status.last_delete.clone()).unwrap();
    assert_eq!((s.deleted, s.refused), (1, 0), "{s:?}");
    assert!(!root.join("junk").exists());
    e.shutdown(Duration::from_secs(1));
}
