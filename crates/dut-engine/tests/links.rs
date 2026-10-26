//! Scenario 11 of `docs/data-pipeline.md` §10.2 on a real tree: a Nix-like store whose files
//! are hard-linked from many profiles, scanned, then the profiles rescanned, so the rescan's
//! links are merged into the visible ones when it swaps in. Every inode must end with exactly
//! one charged link, and the totals must count each inode once. The simulation runs the same
//! scenario with 2,000 links (`crates/dut-sim/tests/scenarios.rs`); this runs it with the
//! engine's real threads and up to a million links.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use dut_engine::{Command, Engine, EngineOptions, Model, Timing};
use dut_scan::JobOptions;
use dut_tree::{Ix, NodeFlags};

const FILE_BYTES: usize = 3000;

/// `store/s{i}` for `inodes` files, and `profiles/p{p}/l{i}` linking each of them, for
/// `profiles` profiles.
fn make_tree(root: &Path, inodes: usize, profiles: usize) {
    let store = root.join("store");
    fs::create_dir(&store).unwrap();
    for i in 0..inodes {
        fs::write(store.join(format!("s{i}")), vec![7u8; FILE_BYTES]).unwrap();
    }
    std::thread::scope(|s| {
        for t in 0..4 {
            let store = &store;
            s.spawn(move || {
                for p in (t..profiles).step_by(4) {
                    let d = root.join(format!("profiles/p{p}"));
                    fs::create_dir_all(&d).unwrap();
                    for i in 0..inodes {
                        fs::hard_link(store.join(format!("s{i}")), d.join(format!("l{i}")))
                            .unwrap();
                    }
                }
            });
        }
    });
}

fn child(m: &Model, ix: Ix, name: &str) -> Ix {
    m.tree
        .children(ix)
        .find(|&c| m.tree.name(c) == name.as_bytes())
        .unwrap_or_else(|| panic!("no {name}"))
}

/// Charged links and all hard links below `ix`.
fn count_links(m: &Model, ix: Ix) -> (u64, u64) {
    let (mut charged, mut links) = (0, 0);
    let mut stack = vec![ix];
    while let Some(ix) = stack.pop() {
        let f = m.tree.flags(ix);
        if f.has(NodeFlags::HARDLINK) {
            links += 1;
            charged += f.has(NodeFlags::CHARGED) as u64;
        }
        stack.extend(m.tree.children(ix));
    }
    (charged, links)
}

/// Peak resident memory of this process, from `/proc` (Linux only).
fn peak_rss_mib() -> Option<u64> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024)
}

/// Write-lock holds between two readings of the timing: p99 and longest, as bucket bounds.
fn holds(from: &Timing, to: &Timing) -> String {
    let mut h = [0; Timing::BUCKETS];
    for (k, n) in h.iter_mut().enumerate() {
        *n = to.hold[k] - from.hold[k];
    }
    let longest = h.iter().rposition(|&n| n > 0).unwrap_or(0);
    format!(
        "{} slices, hold p99 < {:?}, longest < {:?}",
        h.iter().sum::<u64>(),
        Timing::percentile(&h, 0.99),
        Duration::from_micros(1 << longest)
    )
}

fn check(inodes: usize, profiles: usize) {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let t = Instant::now();
    make_tree(tmp.path(), inodes, profiles);
    let links = (inodes * profiles) as u64;
    eprintln!(
        "{inodes} inodes, {links} links in {profiles} profiles, made in {:?}",
        t.elapsed()
    );

    let e = Engine::start(
        dut_scan::native_backend(dut_sys::storage(tmp.path())),
        // The tree is checked once at the end: checking after every slice would cost more than
        // the scan and hide the applier's own lock hold times.
        EngineOptions {
            check_invariants: false,
            ..Default::default()
        },
    );
    let t = Instant::now();
    e.send(Command::Scan {
        path: tmp.path().to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(600))));
    let timing = |e: &Engine<_>| e.read(|m| m.status.timing.clone());
    let t_scan = timing(&e);
    eprintln!(
        "scan: {:?}; {}",
        t.elapsed(),
        holds(&Timing::default(), &t_scan)
    );

    let (root_alloc, profiles_ref) = e.read(|m| {
        let root = m.tree.roots().next().unwrap();
        let st = &m.status.stats;
        assert_eq!((st.corrupt, st.lost, st.duplicate), (0, 0, 0));
        assert!(!m.status.incomplete);
        // store/s{i} and each link: inodes * (profiles + 1) hard links, one charge per inode.
        assert_eq!(
            count_links(m, root),
            (inodes as u64, inodes as u64 * (profiles as u64 + 1))
        );
        let p = child(m, root, "profiles");
        (m.tree.total(root).alloc, m.tree.node_ref(p))
    });

    let t = Instant::now();
    e.send(Command::Rescan { node: profiles_ref });
    // The job finishes (its own links settled), then the swap frees the old subtree and merges
    // the links into the visible ones, in steps.
    while e.read(|m| m.status.commands_applied) < 2 || e.read(|m| m.status.running()) > 0 {
        assert!(t.elapsed() < Duration::from_secs(600), "rescan did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    let (job_done, t_job) = (t.elapsed(), timing(&e));
    assert!(e.wait_idle(Some(Duration::from_secs(600))));
    let settled = t.elapsed();
    let t_end = timing(&e);
    eprintln!(
        "rescan of {links} links: job {job_done:?} ({}), settled {settled:?} ({}); peak RSS \
         {:?} MiB",
        holds(&t_scan, &t_job),
        holds(&t_job, &t_end),
        peak_rss_mib()
    );

    e.read(|m| {
        let root = m.tree.roots().next().unwrap();
        let st = &m.status.stats;
        assert_eq!((st.corrupt, st.lost, st.duplicate), (0, 0, 0));
        assert!(!m.status.incomplete);
        assert_eq!(
            count_links(m, root),
            (inodes as u64, inodes as u64 * (profiles as u64 + 1))
        );
        assert_eq!(m.tree.total(root).alloc, root_alloc);
        if let Err(v) = m.tree.check(&[]) {
            panic!("{v:?}");
        }
        let profiles = child(m, root, "profiles");
        assert_eq!(
            m.tree.total(profiles).items,
            1 + links + profiles_dirs(m, profiles)
        );
    });
    e.shutdown(Duration::from_secs(1));
}

fn profiles_dirs(m: &Model, profiles: Ix) -> u64 {
    m.tree.children(profiles).count() as u64
}

#[test]
fn links_swapped_in() {
    check(200, 50);
}

/// A million links to 2,000 inodes, 501 links each. Run with
/// `cargo test --release -p dut-engine --test links -- --ignored --nocapture`.
#[test]
#[ignore]
fn million_links_swapped_in() {
    check(2000, 500);
}
