use dfdu_scan::JobId;

use super::*;
use crate::test_util::{quiet, settled, small};
use crate::{SimConfig, SimError};

/// A run settles once nothing is running or queued, and its end checks the tree against the
/// spec: every directory is compared.
#[test]
fn a_run_ends_checked_against_its_spec() {
    let sim = Sim::with_spec(2, &quiet(), small());
    assert!(!sim.settled(), "the scan is still to run");
    let sim = settled(&quiet());
    assert!(sim.settled());
    let report = sim.finish().unwrap();
    assert_eq!(report.dirs, 3, "the root, d and e");
    assert!(report.ticks > 0);
}

/// What a cancel leaves incomplete is rescanned until nothing is, as a user would.
#[test]
fn what_a_cancel_leaves_is_rescanned() {
    let mut sim = Sim::with_spec(2, &quiet(), small());
    sim.command(Command::Cancel { job: JobId(1) }).unwrap();
    sim.settle().unwrap();
    assert!(sim.report.recoveries >= 1);
    assert_eq!(sim.finish().unwrap().dirs, 3);
}

/// A run that does not settle within its ticks fails, saying what is still going.
#[test]
fn a_run_out_of_ticks_fails() {
    let cfg = SimConfig {
        max_ticks: 0,
        ..quiet()
    };
    let mut sim = Sim::with_spec(4, &cfg, small());
    let SimError { seed, ticks, what } = sim.settle().unwrap_err();
    assert_eq!((seed, ticks), (4, Some(0)));
    assert!(what.starts_with("did not finish: running jobs 1"), "{what}");
}

/// The end checks the most budget the run used against the config's.
#[test]
fn the_end_checks_the_budget() {
    let mut sim = settled(&quiet());
    sim.cfg.budget = 0;
    let e = sim.finish().unwrap_err();
    assert!(e.what.starts_with("budget exceeded: "), "{e}");
    assert!(e.ticks.is_some());
}
