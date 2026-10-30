//! Resuming a tree loaded from a file with the directories a change journal names: each is
//! listed again, the loaded subdirectories it still has are kept, and what is new is scanned.
//! The journal's answers are given by hand; the trees are on disk.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode, Timing};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::{DirState, Kind, Meta, NodeRef, Tree};

mod common;
#[cfg(unix)]
use common::loaded::{OLD, age, set_mtime};
use common::loaded::{find, loaded, resume, resume_since, scanned, totals, write};
use common::tempdir;

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
        for p in [
            "added",
            "new/deeper/n",
            "renamed/m",
            "renamed/late",
            "deep/a/b/more",
        ] {
            assert!(find(t, p).is_some(), "{p} is scanned");
        }
        for p in ["gone", "old", "link"] {
            assert!(find(t, p).is_none(), "{p} is gone");
        }
        for (r, p) in kept
            .iter()
            .zip(["keep", "keep/sub", "keep/sub/k2", "keep/k1"])
        {
            assert_eq!(t.resolve_visible(*r), find(t, p), "{p} is kept as loaded");
        }
    });
    let got = totals(&e);
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// What the journal does not name is read only when its stat shows it changed: a file
/// rewritten in place stays as loaded, and so does a directory whose mtime is the one it was
/// loaded with (as after a change within the same second), also above a named one, which a
/// rescan of the named one does not list.
#[cfg(unix)]
#[test]
fn what_the_journal_does_not_name_stays_as_loaded() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("quiet/q"), 3_000);
    write(&root.join("a/b/c/f"), 1_000);
    write(&root.join("a/other/o"), 2_000);
    age(&root);
    let tree = loaded(&root);
    let other = tree.node_ref(find(&tree, "a/other").unwrap());

    write(&root.join("quiet/q"), 40_000);
    write(&root.join("a/b/c/new"), 5_000);
    write(&root.join("a/top"), 5_000);
    set_mtime(&root.join("a"), OLD);

    let e = resume(tree, &["a/b/c"]);
    e.read(|m| {
        let t = &m.tree;
        assert!(
            find(t, "a/b/c/new").is_some(),
            "a named directory is listed"
        );
        assert!(find(t, "a/top").is_none(), "the directory above it is not");
        assert_eq!(t.raw_size(find(t, "quiet/q").unwrap()).0, 3_000);
        assert_eq!(t.resolve_visible(other), find(t, "a/other"));
    });
    e.shutdown(Duration::from_secs(1));
}

/// A journal that lost every change (it answered with none): the check of the directories the
/// resume keeps finds those deep in the tree by their stat (entries added and removed, a
/// directory removed, another replaced), and the resume ends as a new scan would. What did not
/// change stays as loaded.
#[cfg(unix)]
#[test]
fn changes_the_journal_lost_are_found_by_their_mtimes() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("quiet/deep/q"), 3_000);
    write(&root.join("a/b/c/f"), 1_000);
    write(&root.join("a/b/old/o"), 2_000);
    write(&root.join("x/y/z/gone/g"), 4_000);
    write(&root.join("x/y/swap/s"), 5_000);
    age(&root);
    let tree = loaded(&root);
    let quiet = tree.node_ref(find(&tree, "quiet/deep").unwrap());

    write(&root.join("a/b/c/new"), 6_000);
    write(&root.join("a/b/c/d/e/deeper"), 8_000);
    fs::remove_file(root.join("a/b/old/o")).unwrap();
    fs::remove_dir_all(root.join("x/y/z/gone")).unwrap();
    fs::remove_dir_all(root.join("x/y/swap")).unwrap();
    write(&root.join("x/y/swap/t"), 7_000);

    let e = resume(tree, &[]);
    e.read(|m| {
        let t = &m.tree;
        for p in ["a/b/c/new", "a/b/c/d/e/deeper", "x/y/swap/t"] {
            assert!(find(t, p).is_some(), "{p} is scanned");
        }
        for p in ["a/b/old/o", "x/y/z/gone", "x/y/swap/s"] {
            assert!(find(t, p).is_none(), "{p} is gone");
        }
        assert_eq!(t.resolve_visible(quiet), find(t, "quiet/deep"));
    });
    let got = totals(&e);
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// A directory changed in the second the journal position was read or later is listed again
/// whatever its mtime says: a change in that same second after its scan read it does not move
/// it. Here every directory was, and the changes the journal lost are all found.
#[test]
fn what_changed_since_the_journal_position_is_listed_again() {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("a/b/f"), 1_000);
    write(&root.join("a/c/g"), 2_000);
    let tree = loaded(&root);

    write(&root.join("a/b/new"), 6_000);
    fs::remove_file(root.join("a/c/g")).unwrap();

    let e = resume_since(tree, JobOptions::default(), &[], since);
    let got = totals(&e);
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// A tree loaded from a file refuses deletes, native commands and rescans until a resume has
/// taken it over: its sizes may be old anywhere below, and the options to scan it with are the
/// resume's. Then the delete goes ahead.
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
    e.send(Command::Rescan { node: junk });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    e.read(|m| {
        let refused: Vec<_> = m
            .status
            .notices
            .iter()
            .filter(|n| n.contains("not refreshed yet"))
            .collect();
        assert_eq!(refused.len(), 4, "{:?}", m.status.notices);
        assert!(refused[0].starts_with("delete: junk "), "{refused:?}");
        assert!(refused[2].starts_with("native command: "), "{refused:?}");
        assert!(refused[3].starts_with("rescan: "), "{refused:?}");
    });
    assert!(root.join("junk/big").exists() && root.join("file").exists());

    e.send(Command::Resume {
        node,
        opts: JobOptions::default(),
        changed: Some(vec![]),
        since: 0,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
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

/// The longest write-lock slice while a resume takes over a loaded tree of about a million
/// entries that the journal says did not change (`--ignored`, release): the resume walks the
/// whole tree, classifying it, a step at a time, while the workers stat its 111,101
/// directories, which are on disk (the files are not), and find them as loaded.
#[test]
#[ignore = "a timing run"]
fn resume_lock_hold_on_a_large_tree() {
    let dir = tempdir();
    let root = dir.path().to_path_buf();
    let mut t = Tree::default();
    let meta = Meta::from_metadata(&fs::metadata(&root).unwrap());
    let r = t.new_root(&dfdu_tree::name_bytes(root.as_os_str()), &meta, true);
    let names = ["target", "node_modules", "src", "build", ".git", "cache"];
    let files = [
        "Cargo.toml",
        "package.json",
        "a.rs",
        "b.o",
        "c.txt",
        "d",
        "e",
        "f",
        "g",
    ];
    let leaf = |k: usize| format!("{}{}", names[k % names.len()], k / names.len());
    for i in 0..100 {
        for j in 0..100 {
            for k in 0..10 {
                fs::create_dir_all(root.join(format!("d{i}/e{j}/{}", leaf(k)))).unwrap();
            }
        }
    }
    let mut ino = u64::MAX / 2;
    let mut file = || {
        ino += 1;
        Meta {
            kind: Kind::File,
            apparent: 100,
            alloc: 4096,
            mtime: 1,
            dev: meta.dev,
            ino,
            nlink: 1,
            mount: 0,
        }
    };
    let on_disk = |p: &std::path::Path| Meta::from_metadata(&fs::metadata(p).unwrap());
    let mut dirs = vec![r];
    for i in 0..100 {
        let pa = root.join(format!("d{i}"));
        let a = t.add_dir(r, format!("d{i}").as_bytes(), &on_disk(&pa));
        dirs.push(a);
        for j in 0..100 {
            let pb = pa.join(format!("e{j}"));
            let b = t.add_dir(a, format!("e{j}").as_bytes(), &on_disk(&pb));
            dirs.push(b);
            for k in 0..10 {
                let pc = pb.join(leaf(k));
                let c = t.add_dir(b, leaf(k).as_bytes(), &on_disk(&pc));
                dirs.push(c);
                for f in files {
                    t.add_file(c, f.as_bytes(), &file(), true, None);
                }
            }
        }
    }
    for d in dirs {
        let r = t.dir_mut(d).unwrap();
        r.state = DirState::Complete;
        r.outcome = r.state;
        r.listing_closed = true;
        r.reported = true;
    }
    let n = t.live_nodes();
    let node = t.node_ref(r);
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            measure_workers: 0,
            ..Default::default()
        },
    );
    e.load(t);
    let (rules, _) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    let class = dfdu_scan::classify::ClassOpts::for_root(std::sync::Arc::new(rules), &root);
    let before = e.read(|m| m.status.timing.clone());
    let started = std::time::Instant::now();
    e.send(Command::Resume {
        node,
        opts: JobOptions {
            class: Some(class),
            ..JobOptions::default()
        },
        changed: Some(vec![]),
        since: 0,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(600))).is_idle());
    let took = started.elapsed();
    let timing = e.read(|m| m.status.timing.clone());
    let mut hold = timing.hold;
    for (h, b) in hold.iter_mut().zip(before.hold) {
        *h -= b;
    }
    let longest = Timing::percentile(&hold, 1.0);
    let p99 = Timing::percentile(&hold, 0.99);
    // The resume's own job, and the rescan of the root, whose mtime moved as it was filled.
    let jobs = e.read(|m| m.status.jobs.len());
    eprintln!(
        "{n} nodes resumed in {took:?}: {} slices, longest write-lock hold under {longest:?}, \
         p99 under {p99:?}, {jobs} jobs",
        timing.slices - before.slices
    );
    assert!(jobs <= 2, "a directory found changed");
    e.shutdown(Duration::from_secs(1));
}
