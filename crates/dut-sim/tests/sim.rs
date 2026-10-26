//! Seeded simulation runs (`docs/data-pipeline.md` §10.1). Set `DUT_SIM_SEEDS` to run more
//! seeds and `DUT_SIM_SEED` to rerun one.

use dut_sim::{SimConfig, SpecConfig, run};

fn quiet_injected_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload().downcast_ref::<&str>().copied().unwrap_or("");
        if msg != "injected panic" {
            default(info);
        }
    }));
}

fn seeds(default: u64) -> Vec<u64> {
    if let Ok(s) = std::env::var("DUT_SIM_SEED") {
        return vec![s.parse().expect("DUT_SIM_SEED")];
    }
    let n = std::env::var("DUT_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default);
    (0..n).collect()
}

fn run_all(name: &str, cfg: &SimConfig, default_seeds: u64) -> dut_sim::SimReport {
    quiet_injected_panics();
    let mut failures = vec![];
    let mut t = dut_sim::SimReport::default();
    for seed in seeds(default_seeds) {
        match run(seed, cfg) {
            Ok(r) => {
                t.dirs += r.dirs;
                t.abandoned += r.abandoned;
                t.rescans += r.rescans;
                t.deletes += r.deletes;
                t.deleted_items += r.deleted_items;
                t.cancels += r.cancels;
                t.recoveries += r.recoveries;
                t.stalled_msgs += r.stalled_msgs;
                t.late += r.late;
                t.discarded_cut += r.discarded_cut;
                t.subvols += r.subvols;
                t.ro_subvols += r.ro_subvols;
                t.dup_mounts += r.dup_mounts;
                t.cross_runs += r.cross_runs;
                t.network_skipped += r.network_skipped;
                t.kernel_skipped += r.kernel_skipped;
                t.measured += r.measured;
                t.shared_bytes += r.shared_bytes;
                t.chunk_tasks += r.chunk_tasks;
                t.gates += r.gates;
                t.deferred += r.deferred;
                t.revoked += r.revoked;
                t.at_limit += r.at_limit;
                t.max_budget_used = t.max_budget_used.max(r.max_budget_used);
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

/// The default run must keep exercising the risky paths, or it proves little.
#[test]
fn coverage_of_risky_paths() {
    let t = run_all("coverage", &SimConfig::default(), 300);
    assert!(t.rescans > 100, "rescans {}", t.rescans);
    assert!(t.discarded_cut > 0, "no message was discarded under a cut");
    assert!(
        t.stalled_msgs > 0 && t.abandoned > 0,
        "stalls not exercised"
    );
    assert!(t.late > 0, "no late message after an abandon");
    assert!(
        t.subvols > 100 && t.ro_subvols > 20 && t.dup_mounts > 5 && t.cross_runs > 50,
        "mounts and subvolumes not exercised: {t:?}"
    );
    assert!(
        t.network_skipped > 5 && t.kernel_skipped > 5,
        "network and kernel mounts not exercised: {t:?}"
    );
    assert!(
        t.measured > 1000 && t.shared_bytes > 0,
        "shared extents not exercised: {t:?}"
    );
    assert!(t.chunk_tasks > 1000, "chunk tasks {}", t.chunk_tasks);
    assert!(
        t.deletes > 100 && t.deleted_items > 100,
        "deletes {} deleted {}",
        t.deletes,
        t.deleted_items
    );
}

#[test]
fn faults_and_rescans() {
    run_all("faults_and_rescans", &SimConfig::default(), 300);
}

#[test]
fn no_faults_is_complete() {
    let cfg = SimConfig {
        spec: SpecConfig {
            faults: false,
            ..Default::default()
        },
        ..Default::default()
    };
    run_all("no_faults", &cfg, 200);
}

/// Without mount ids (Linux before 5.8, other systems) `dev` decides: subvolumes are skipped as
/// other filesystems, the pre-statx behaviour.
#[test]
fn without_mount_ids() {
    let cfg = SimConfig {
        spec: SpecConfig {
            mount_ids: false,
            ..Default::default()
        },
        cross_mounts: Some(false),
        ..Default::default()
    };
    let t = run_all("without_mount_ids", &cfg, 100);
    assert_eq!(t.subvols, 0, "subvolumes are not opened without mount ids");
}

#[test]
fn one_worker() {
    run_all(
        "one_worker",
        &SimConfig {
            workers: 1,
            ..Default::default()
        },
        100,
    );
}

#[test]
fn many_workers_tiny_budget() {
    let cfg = SimConfig {
        workers: 12,
        budget: 1,
        ..Default::default()
    };
    run_all("many_workers_tiny_budget", &cfg, 100);
}

#[test]
fn wide_directories() {
    let cfg = SimConfig {
        spec: SpecConfig {
            max_depth: 2,
            max_dirs: 3,
            max_files: 60,
            ..Default::default()
        },
        ..Default::default()
    };
    run_all("wide_directories", &cfg, 100);
}

/// Every block stat'ed by its listing task: the path without chunk tasks.
#[test]
fn no_chunk_tasks() {
    let mut cfg = SimConfig::default();
    cfg.limits.chunk_credit = 0;
    let t = run_all("no_chunk_tasks", &cfg, 100);
    assert_eq!(t.chunk_tasks, 0);
}

/// Many mounts on spinning disks and network storage, all scanned, with small limits and more
/// workers than they allow (`docs/data-pipeline.md` §4.5): tasks wait for permits, stuck parts
/// give theirs back, and totals still match.
#[test]
fn mount_limits() {
    let cfg = SimConfig {
        workers: 5,
        spec: SpecConfig {
            mount_per_mille: 300,
            ..Default::default()
        },
        cross_mounts: Some(true),
        network: Some(true),
        ..Default::default()
    };
    let t = run_all("mount_limits", &cfg, 200);
    assert!(
        t.gates > 200 && t.deferred > 1000 && t.revoked > 10 && t.at_limit > 1000,
        "mount limits not exercised: {t:?}"
    );
}
