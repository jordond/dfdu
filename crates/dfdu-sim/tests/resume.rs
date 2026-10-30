//! Seeded resumed runs ([`run_resumed`]): a scan, a snapshot of it, changes, and a resume with
//! what a change journal says, among random rescans, deletes and cancels, checked against the
//! spec. Set `DFDU_SIM_SEEDS` to run more seeds and `DFDU_SIM_SEED` to rerun one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dfdu_sim::{SimConfig, SimReport, SpecConfig, quiet_injected_panics, run_resumed, sim_rules};

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
        match run_resumed(seed, cfg) {
            Ok(r) => {
                t.dirs += r.dirs;
                t.rescans += r.rescans;
                t.deletes += r.deletes;
                t.cancels += r.cancels;
                t.recoveries += r.recoveries;
                t.classified += r.classified;
                t.kept += r.kept;
                t.lost_events += r.lost_events;
                t.full_resumes += r.full_resumes;
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
fn resumed() {
    let t = run_all("resumed", &SimConfig::default(), 300);
    if std::env::var("DFDU_SIM_SEED").is_err() {
        assert!(
            t.kept > 1000 && t.lost_events > 10 && t.full_resumes > 5,
            "resumes not exercised: {t:?}"
        );
        assert!(
            t.cancels > 20 && t.rescans > 50 && t.deletes > 50,
            "commands not exercised: {t:?}"
        );
    }
}

/// Classes against the reference classifier, with names the built-in rules match: a kept
/// directory keeps its loaded class only while its new listing gives it. Nothing is deleted,
/// as in the classification runs.
#[test]
fn resumed_classes() {
    let cfg = SimConfig {
        deletes: 0,
        rules: Some(sim_rules()),
        spec: SpecConfig {
            rule_names: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let t = run_all("resumed_classes", &cfg, 300);
    if std::env::var("DFDU_SIM_SEED").is_err() {
        assert!(t.classified > 300 && t.kept > 1000, "not exercised: {t:?}");
    }
}

/// More cancels and rescans, so they meet the resume's walk and its rescans.
#[test]
fn resumed_with_many_commands() {
    let cfg = SimConfig {
        rescans: 8,
        cancels: 4,
        ..Default::default()
    };
    run_all("resumed_with_many_commands", &cfg, 200);
}
