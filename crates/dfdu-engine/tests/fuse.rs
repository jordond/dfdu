//! A hung mount does not stop the scan: a FUSE filesystem (`dfdu-hangfs`) whose selected
//! directories never answer `readdir` and whose selected file never answers `getattr`, mounted as a
//! network filesystem below the scan root and scanned with `network`. The scan finishes with those
//! directories reported stalled and then abandoned, everything else counted exactly, at most the
//! mount's limit of requests being answered at once, and no more worker threads than the pool, the
//! mount's workers and one per stuck call.
//!
//! Needs `/dev/fuse` and root (or `fusermount3`); skipped with a message otherwise, unless
//! `DFDU_FUSE_REQUIRED` is set (CI).
#![cfg(target_os = "linux")]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions, Model};
use dfdu_hangfs::{Mounted, Options, Spec};
use dfdu_scan::{JobOptions, Limits, NativeBackend};
use dfdu_tree::{DirState, Ix, Tree};

/// Mounts `spec` at `at`, or explains why it cannot and returns `None`.
fn mount(spec: Spec, at: &Path, opts: Options) -> Option<Mounted> {
    match Mounted::mount(spec, at, opts) {
        Ok(m) => Some(m),
        Err(e) if std::env::var_os("DFDU_FUSE_REQUIRED").is_none() => {
            eprintln!("skipped: cannot mount a FUSE filesystem ({e}); needs /dev/fuse and root");
            None
        }
        Err(e) => panic!("cannot mount a FUSE filesystem: {e}"),
    }
}

/// Worker threads of this process.
fn workers() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|rd| {
            rd.flatten()
                .filter(|t| {
                    std::fs::read_to_string(t.path().join("comm"))
                        .is_ok_and(|c| c.trim() == "dfdu-worker")
                })
                .count()
        })
        .unwrap_or(0)
}

fn find(tree: &Tree, root: Ix, rel: &str) -> Option<Ix> {
    let mut cur = root;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        cur = tree
            .children(cur)
            .find(|&c| tree.name(c) == part.as_bytes())?;
    }
    Some(cur)
}

/// `(apparent, alloc, items)` of every path below the mount, from the spec, with the
/// directories in `lost` counted as entries but not read.
fn expected(spec: &Spec, lost: &HashSet<usize>) -> HashMap<String, (u64, u64, u64)> {
    fn total(spec: &Spec, n: usize, lost: &HashSet<usize>) -> (u64, u64, u64) {
        let mut t = (spec.nodes[n].size, spec.alloc(n), 1);
        if !lost.contains(&n) {
            for &c in &spec.nodes[n].children {
                let ct = total(spec, c, lost);
                t = (t.0 + ct.0, t.1 + ct.1, t.2 + ct.2);
            }
        }
        t
    }
    let mut under_lost = HashSet::new();
    let paths = spec.paths();
    for (p, n) in &paths {
        if lost.contains(n) {
            under_lost.insert(p.clone());
        }
    }
    paths
        .iter()
        .filter(|(p, _)| {
            !under_lost.iter().any(|l| {
                p.len() > l.len() && p.starts_with(l.as_str()) && p[l.len()..].starts_with('/')
            })
        })
        .map(|(p, n)| (p.clone(), total(spec, *n, lost)))
        .collect()
}

fn totals_below(m: &Model, root: Ix) -> HashMap<String, (u64, u64, u64)> {
    let tree = &m.tree;
    let base = tree.path(root).len();
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        let p = tree.path(ix);
        let rel = String::from_utf8_lossy(&p[base..])
            .trim_start_matches('/')
            .to_string();
        let t = tree.total(ix);
        out.insert(rel, (t.apparent, t.alloc, t.items));
        stack.extend(tree.children(ix));
    }
    out
}

#[test]
fn hung_network_mount_is_abandoned_and_the_rest_counted() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("local"), vec![1u8; 10_000]).unwrap();
    let at = root.join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (spec, hung) = Spec::hanging(30);
    let opts = Options {
        // The mount table shows `fuse.sshfs`: a network filesystem, limited to 4 parts.
        subtype: Some("sshfs".into()),
        delay: Duration::from_millis(1),
    };
    let Some(fs) = mount(spec.clone(), &at, opts) else {
        return;
    };

    let pool = 2;
    let limits = Limits {
        stall_ns: 300_000_000,
        ..Limits::default()
    };
    let engine = Engine::start(
        NativeBackend::default(),
        EngineOptions {
            workers: pool,
            limits,
            auto_abandon: Some(Duration::from_millis(300)),
            check_invariants: true,
            measure_workers: 0,
            ..Default::default()
        },
    );
    let t0 = Instant::now();
    engine.send(Command::Scan {
        path: root,
        opts: JobOptions {
            network: true,
            ..JobOptions::default()
        },
    });
    // Until the scan ends: the most worker threads seen, and which hung directories were
    // seen stalled before they were abandoned.
    let mut max_threads = 0;
    let mut seen_stalled = HashSet::new();
    loop {
        max_threads = max_threads.max(workers());
        engine.read(|m| {
            let Some(r) = m.tree.roots().next() else {
                return;
            };
            for h in &hung {
                if let Some(ix) = find(&m.tree, r, &format!("mnt/{h}"))
                    && m.tree
                        .dir(ix)
                        .is_some_and(|d| d.stalled && d.state == DirState::Pending)
                {
                    seen_stalled.insert(*h);
                }
            }
        });
        if engine.wait_idle(Some(Duration::from_millis(5))).is_idle() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the scan did not finish"
        );
    }
    let elapsed = t0.elapsed();
    max_threads = max_threads.max(workers());

    // The hung calls are still stuck: the scan finished without them.
    assert_eq!(fs.held(), hung.len(), "requests still unanswered");
    let lost: HashSet<usize> = spec
        .paths()
        .into_iter()
        .filter(|(p, _)| hung.contains(&p.as_str()))
        .map(|(_, n)| n)
        .collect();
    let want = expected(&spec, &lost);
    engine.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let mnt = find(&m.tree, r, "mnt").expect("mount point in the tree");
        for h in &hung {
            let ix = find(&m.tree, mnt, h).unwrap();
            assert_eq!(m.tree.dir(ix).unwrap().state, DirState::Abandoned, "{h}");
        }
        let got = totals_below(m, mnt);
        for (p, w) in &want {
            assert_eq!(got.get(p), Some(w), "totals of mnt/{p}");
        }
        assert_eq!(got.len(), want.len(), "entries below the mount");
        assert!(m.status.stats.stalled >= hung.len() as u64);
        assert_eq!(m.status.stats.corrupt + m.status.stats.lost, 0);
    });
    assert_eq!(
        seen_stalled.len(),
        hung.len(),
        "stalled before abandoned: {seen_stalled:?}"
    );

    // The mount is limited to 4 parts at once and adds 4 workers; each stuck call may cost
    // one more thread, up to a cap.
    let gates = engine.scanner().gate_stats();
    assert_eq!(gates.len(), 1, "{gates:?}");
    let g = &gates[0];
    assert_eq!(g.limit, 4);
    assert!(g.revoked >= hung.len() as u64, "{g:?}");
    let mount_workers = engine.scanner().mount_workers();
    assert_eq!(mount_workers, 4);
    let bound = pool + mount_workers + hung.len().min(pool.min(16) + 64);
    assert!(
        max_threads <= bound,
        "{max_threads} worker threads, at most {bound} allowed"
    );
    let peak = fs.peak_in_flight();
    assert!(
        (2..=4).contains(&peak),
        "{peak} requests on the mount at once (limit 4)"
    );
    eprintln!(
        "scan with 3 hung calls: {elapsed:?}, {max_threads} worker threads at most \
         (bound {bound}), {peak} requests at once on the mount, gate {g:?}"
    );

    // Teardown with threads stuck in the kernel: answering the kept requests lets them
    // return and exit, then the filesystem unmounts.
    fs.release();
    engine.shutdown(Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(5);
    while workers() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(workers(), 0, "worker threads left after release");
    drop(fs);
    assert_eq!(std::fs::read_dir(&at).unwrap().count(), 0, "still mounted");
}
