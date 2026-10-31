//! The longest write-lock slices while a resume takes over large loaded trees (`--ignored`,
//! release): timing runs, beside the checks of what a resume does (`resume.rs`). The
//! directories are on disk, the files in the loaded trees alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dfdu_engine::{Engine, EngineOptions, Timing};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::{DirState, Ix, Kind, Meta, Tree};

mod common;
use common::journal::resume_changed;
use common::loaded::{find, loaded};
use common::tempdir;

/// Files of a loaded tree that are not on disk, on device `dev`, each an inode of its own.
struct Files {
    dev: u64,
    ino: u64,
}

impl Files {
    fn on(dev: u64) -> Files {
        Files {
            dev,
            ino: u64::MAX / 2,
        }
    }

    /// Adds file `name` to directory `dir`, its totals with it.
    fn add(&mut self, t: &mut Tree, dir: Ix, name: &[u8]) {
        self.ino += 1;
        let meta = Meta {
            kind: Kind::File,
            apparent: 100,
            alloc: 4096,
            mtime: 1,
            dev: self.dev,
            ino: self.ino,
            nlink: 1,
            mount: 0,
        };
        let f = t.add_file(dir, name, &meta, true, None);
        let own = t.own(f);
        t.propagate_add(dir, &own);
    }
}

/// What a timed resume took.
struct Run {
    took: Duration,
    slices: u64,
    longest: Duration,
    p99: Duration,
}

/// Loads `tree`, the tree of `root`, and resumes it classifying with the built-in rules, the
/// journal naming `changed`; waits until it is done.
fn timed_resume(tree: Tree, root: &Path, changed: Vec<PathBuf>) -> (Engine<PortableBackend>, Run) {
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            measure_workers: 0,
            ..Default::default()
        },
    );
    let (rules, _) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    let class = ClassOpts::for_root(Arc::new(rules), root);
    let before = e.read(|m| m.status.timing.clone());
    let started = Instant::now();
    let opts = JobOptions {
        class: Some(class),
        ..JobOptions::default()
    };
    resume_changed(&e, tree, opts, changed, 0);
    assert!(e.wait_idle(Some(Duration::from_secs(600))).is_idle());
    let took = started.elapsed();
    let timing = e.read(|m| m.status.timing.clone());
    let mut hold = timing.hold;
    for (h, b) in hold.iter_mut().zip(before.hold) {
        *h -= b;
    }
    let run = Run {
        took,
        slices: timing.slices - before.slices,
        longest: Timing::percentile(&hold, 1.0),
        p99: Timing::percentile(&hold, 0.99),
    };
    (e, run)
}

/// `root` with `d{i}/e{j}/{leaf}` directories on disk, 100 by 100 by 10, the leaves named as
/// projects and caches are, and the tree loaded from a file that saw nine files in each leaf.
fn wide_project_tree(root: &Path) -> Tree {
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
    let on_disk = |p: &Path| Meta::from_metadata(&fs::metadata(p).unwrap());
    let mut t = Tree::default();
    let top = t.new_root(
        &dfdu_tree::name_bytes(root.as_os_str()),
        &on_disk(root),
        true,
    );
    let mut files_of = Files::on(on_disk(root).dev);
    let mut dirs = vec![top];
    for i in 0..100 {
        let pa = root.join(format!("d{i}"));
        let da = t.add_dir(top, format!("d{i}").as_bytes(), &on_disk(&pa));
        dirs.push(da);
        for j in 0..100 {
            let pb = pa.join(format!("e{j}"));
            let db = t.add_dir(da, format!("e{j}").as_bytes(), &on_disk(&pb));
            dirs.push(db);
            for k in 0..10 {
                let dc = t.add_dir(db, leaf(k).as_bytes(), &on_disk(&pb.join(leaf(k))));
                dirs.push(dc);
                for name in files {
                    files_of.add(&mut t, dc, name.as_bytes());
                }
            }
        }
    }
    for d in dirs {
        let rec = t.dir_mut(d).unwrap();
        rec.state = DirState::Complete;
        rec.outcome = rec.state;
        rec.listing_closed = true;
        rec.reported = true;
    }
    t
}

/// The longest write-lock slice while a resume takes over a loaded tree of about a million
/// entries that the journal says did not change: the resume walks the whole tree, classifying
/// it, a step at a time, while the workers stat its 111,101 directories and find them as
/// loaded.
#[test]
#[ignore = "a timing run"]
fn resume_lock_hold_on_a_large_tree() {
    let dir = tempdir();
    let root = dir.path().to_path_buf();
    let t = wide_project_tree(&root);
    let n = t.live_nodes();
    let (e, run) = timed_resume(t, &root, Vec::new());
    // The resume's own job, and the rescan of the root, whose mtime moved as it was filled.
    let jobs = e.read(|m| m.status.jobs.len());
    eprintln!(
        "{n} nodes resumed in {:?}: {} slices, longest write-lock hold under {:?}, p99 under \
         {:?}, {jobs} jobs",
        run.took, run.slices, run.longest, run.p99
    );
    assert!(jobs <= 2, "a directory found changed");
    e.shutdown(Duration::from_secs(1));
}

/// The longest write-lock slice while a resume lists again thousands of directories the
/// journal names: 2,000 below the root, each keeping two subdirectories, 2,000 in a directory
/// of 100,000 entries, and two named directories of their own, one of 100,000 entries and one
/// of 100,000 subdirectories, which it keeps. A listing again finds the loaded files gone.
#[test]
#[ignore = "a timing run"]
fn resume_lock_hold_with_many_changed_directories() {
    const WIDE: usize = 100_000;
    const KEPT: usize = 100_000;
    let dir = tempdir();
    let root = dir.path().to_path_buf();
    let mut changed = Vec::new();
    for i in 0..50 {
        for j in 0..40 {
            for k in ["x", "y"] {
                fs::create_dir_all(root.join(format!("d{i}/e{j}/{k}"))).unwrap();
            }
            changed.push(PathBuf::from(format!("d{i}/e{j}")));
        }
    }
    for k in 0..2_000 {
        fs::create_dir_all(root.join(format!("wide/s{k}"))).unwrap();
        changed.push(format!("wide/s{k}").into());
    }
    fs::create_dir_all(root.join("files")).unwrap();
    changed.push("files".into());
    for k in 0..KEPT {
        fs::create_dir_all(root.join(format!("kept/k{k}"))).unwrap();
    }
    changed.push("kept".into());
    let mut t = loaded(&root);
    let mut files = Files::on(Meta::from_metadata(&fs::metadata(&root).unwrap()).dev);
    for at in ["wide", "files"] {
        let d = find(&t, at).unwrap();
        for k in 0..WIDE - t.children(d).count() {
            files.add(&mut t, d, format!("f{k}").as_bytes());
        }
    }
    let n = t.live_nodes();
    let named = changed.len();
    let (e, run) = timed_resume(t, &root, changed);
    eprintln!(
        "{n} nodes, {named} named directories resumed in {:?}: {} slices, longest write-lock \
         hold under {:?}, p99 under {:?}",
        run.took, run.slices, run.longest, run.p99
    );
    e.read(|m| {
        let t = &m.tree;
        assert_eq!(t.children(find(t, "kept").unwrap()).count(), KEPT);
        assert_eq!(t.children(find(t, "files").unwrap()).count(), 0);
        assert_eq!(t.children(find(t, "wide").unwrap()).count(), WIDE);
    });
    e.shutdown(Duration::from_secs(1));
}
