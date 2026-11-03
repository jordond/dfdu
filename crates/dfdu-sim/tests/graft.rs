//! Scans of a folder above another scan's root that keep what it read: seeded runs
//! ([`run_grafted`]) among random rescans, deletes and cancels, and scenarios of the old root
//! unchanged, changed, changed within the second its scan started, removed, or become a mount,
//! each checked against the spec. Set `DFDU_SIM_SEEDS` to run more seeds and `DFDU_SIM_SEED`
//! to rerun one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use dfdu_sim::{
    ROOT, Rng, SimConfig, SimReport, Spec, SpecBuilder, SpecConfig, quiet_injected_panics,
    run_grafted, scan_above, scan_old, sim_rules,
};

use common::{SEEDS, cfg};

fn seeds(default: u64) -> Vec<u64> {
    if let Ok(s) = std::env::var("DFDU_SIM_SEED") {
        return vec![s.parse().expect("DFDU_SIM_SEED")];
    }
    let n = std::env::var("DFDU_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default);
    (0..n).collect()
}

fn run_all(name: &str, cfg: &SimConfig, default_seeds: u64) -> SimReport {
    quiet_injected_panics();
    let mut failures = vec![];
    let mut t = SimReport::default();
    for seed in seeds(default_seeds) {
        match run_grafted(seed, cfg) {
            Ok(r) => {
                t.kept += r.kept;
                t.full_resumes += r.full_resumes;
                t.classified += r.classified;
                t.cancels += r.cancels;
                t.rescans += r.rescans;
                t.deletes += r.deletes;
            }
            Err(e) => failures.push(e),
        }
    }
    eprintln!("{name}: {t:?}");
    assert!(
        failures.is_empty(),
        "{} failures, first: {}",
        failures.len(),
        failures[0]
    );
    t
}

/// Faults, mounts and the random commands, as the default runs have them.
#[test]
fn grafted() {
    let t = run_all("grafted", &SimConfig::default(), 300);
    if std::env::var("DFDU_SIM_SEED").is_err() {
        assert!(
            t.kept > 100 && t.full_resumes > 5,
            "grafts not exercised: {t:?}"
        );
        assert!(
            t.cancels > 20 && t.deletes > 50,
            "commands not exercised: {t:?}"
        );
    }
}

/// Classes against the reference classifier, with names the built-in rules match: what was
/// kept is classified for the new root. Nothing is deleted, as in the classification runs.
#[test]
fn grafted_classes() {
    let cfg = SimConfig {
        deletes: 0,
        rules: Some(sim_rules()),
        spec: SpecConfig {
            rule_names: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let t = run_all("grafted_classes", &cfg, 300);
    if std::env::var("DFDU_SIM_SEED").is_err() {
        assert!(t.classified > 100 && t.kept > 30, "not exercised: {t:?}");
    }
}

/// The spec node numbers of the scenario's folders.
struct Folders {
    c: u32,
    d: u32,
}

/// `/sim` holding `h` (a file with a hard link in `c`) and `a`, which holds `g` and `c`;
/// `c` holds files, `d` (a file and `n`, a file created later) and `e`, empty. Every
/// directory's mtime is 1.
fn spec() -> (Spec, Folders) {
    let mut b = SpecBuilder::new();
    let h = b.file(ROOT, "h", 3000);
    let a = b.dir(ROOT, "a");
    b.file(a, "g", 2000);
    let c = b.dir(a, "c");
    b.files(c, "f", 4);
    b.link(c, "l", h);
    let d = b.dir(c, "d");
    b.file(d, "x", 500);
    let n = b.file(d, "n", 700);
    b.absent(n);
    b.dir(c, "e");
    (b.build(), Folders { c, d })
}

/// The new file in `d`, and `d`'s mtime moved unless `quietly`.
fn add_n(spec: &Spec, f: &Folders, quietly: bool) {
    let n = spec.nodes[f.d as usize]
        .children
        .iter()
        .copied()
        .find(|&i| spec.nodes[i as usize].name == b"n")
        .unwrap();
    spec.restore(n);
    if !quietly {
        spec.touch(f.d);
    }
}

/// `c` and what is below it on a device and mount of their own, as a mount there would be.
fn mounted(spec: &mut Spec, f: &Folders) {
    let mut stack = vec![f.c];
    while let Some(i) = stack.pop() {
        let n = &mut spec.nodes[i as usize];
        n.meta.dev = 2;
        n.meta.mount = 2;
        stack.extend(n.children.clone());
    }
    spec.dev_fs.insert(2, (1, false));
}

/// No mounts entered.
fn config() -> SimConfig {
    SimConfig {
        cross_mounts: Some(false),
        network: Some(false),
        ..cfg()
    }
}

/// For each seed: `c` scanned as from `since`, `change` made, and `/sim` scanned keeping it,
/// with the folders stat'ed in that spec. When `after` gives a spec, the scan runs on it (with
/// `change` made there too): what the folders became after they were stat'ed.
fn run(
    since: u64,
    change: impl Fn(&Spec, &Folders),
    after: impl Fn() -> Option<Spec>,
) -> Vec<SimReport> {
    quiet_injected_panics();
    (0..SEEDS)
        .map(|seed| {
            let (spec, f) = spec();
            let spec = Arc::new(spec);
            let cfg = config();
            let (scanned, rng, _, _) =
                scan_old(seed, Rng::new(seed), &cfg, spec.clone(), f.c, since).unwrap();
            assert!(scanned.is_some(), "the old scan can be kept");
            change(&spec, &f);
            let r = match after() {
                Some(next) => {
                    change(&next, &f);
                    scan_above(seed, rng, &cfg, scanned, &spec, Arc::new(next))
                }
                None => scan_above(seed, rng, &cfg, scanned, &spec.clone(), spec),
            };
            r.unwrap_or_else(|e| panic!("{e}"))
        })
        .collect()
}

/// Unchanged, the old root and every folder below it are kept.
#[test]
fn an_unchanged_old_root_is_kept() {
    for r in run(5, |_, _| {}, || None) {
        assert_eq!((r.kept, r.full_resumes), (3, 0), "c, d and e: {r:?}");
    }
}

/// A folder changed below the old root is listed again with the folders above it; the rest
/// is kept.
#[test]
fn a_changed_folder_below_the_old_root_is_listed_again() {
    for r in run(5, |s, f| add_n(s, f, false), || None) {
        assert_eq!((r.kept, r.full_resumes), (1, 0), "e: {r:?}");
    }
}

/// A folder changed within the second the old scan started, its mtime not older, is listed
/// again though its mtime did not move.
#[test]
fn a_change_within_the_second_the_old_scan_started_is_found() {
    for r in run(1, |s, f| add_n(s, f, true), || None) {
        assert_eq!(r.full_resumes, 0, "{r:?}");
    }
}

/// The old root removed before the new scan stats it: the folder is scanned afresh. Removed
/// after: the resume finds it gone.
#[test]
fn a_removed_old_root_is_not_kept() {
    let remove = |s: &Spec, f: &Folders| s.delete_subtree(f.c);
    for r in run(5, remove, || None) {
        assert_eq!((r.kept, r.full_resumes), (0, 1), "{r:?}");
    }
    let quiet = |_: &Spec, _: &Folders| {};
    for r in run(5, quiet, || {
        let (s, f) = spec();
        s.delete_subtree(f.c);
        Some(s)
    }) {
        assert_eq!((r.kept, r.full_resumes), (0, 0), "{r:?}");
    }
}

/// An old root that became a mount is scanned afresh when the new scan sees it, and left
/// not entered by the resume when it became one after: neither keeps what was read below it.
#[test]
fn an_old_root_become_a_mount_is_not_entered() {
    let quiet = |_: &Spec, _: &Folders| {};
    let mount = || {
        let (mut s, f) = spec();
        mounted(&mut s, &f);
        Some(s)
    };
    for r in run(5, quiet, mount) {
        assert_eq!(
            (r.kept, r.full_resumes),
            (0, 0),
            "seen after the stats: {r:?}"
        );
    }
    // Seen by the stats: the spec stat'ed is the mounted one.
    quiet_injected_panics();
    for seed in 0..SEEDS {
        let (spec, f) = spec();
        let spec = Arc::new(spec);
        let (scanned, rng, _, _) =
            scan_old(seed, Rng::new(seed), &config(), spec.clone(), f.c, 5).unwrap();
        let (mut m, _) = self::spec();
        mounted(&mut m, &f);
        let m = Arc::new(m);
        let r = scan_above(seed, rng, &config(), scanned, &m, m.clone()).unwrap();
        assert_eq!((r.kept, r.full_resumes), (0, 1), "{r:?}");
    }
}
