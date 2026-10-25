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
                t.stalled_msgs += r.stalled_msgs;
                t.late += r.late;
                t.discarded_cut += r.discarded_cut;
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
