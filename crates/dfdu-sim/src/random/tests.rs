use super::*;
use crate::SimConfig;
use crate::test_util::quiet;

/// The state of a run, as far as it shows.
fn state(sim: &Sim) -> String {
    let tree = &sim.model.tree;
    let root = tree.roots().next().map(|r| tree.total(r));
    format!(
        "{:?} {root:?} {} {}",
        sim.report,
        sim.model.status.commands_applied,
        tree.live_nodes()
    )
}

/// A seed gives the same steps: two runs of it are alike after as many steps.
#[test]
fn a_seed_gives_the_same_steps() {
    let cfg = SimConfig::default();
    let mut a = Sim::new(9, &cfg);
    let mut b = Sim::new(9, &cfg);
    for _ in 0..500 {
        a.random_step().unwrap();
        b.random_step().unwrap();
    }
    assert_eq!(a.report.ticks, 500);
    assert_eq!(state(&a), state(&b));
}

/// Commands are issued at random while the config allows them, and no more.
#[test]
fn random_commands_stay_within_the_config() {
    let cfg = SimConfig {
        rescans: 2,
        deletes: 1,
        cancels: 1,
        ..quiet()
    };
    let (mut rescans, mut deletes) = (0, 0);
    for seed in 0..10 {
        let mut sim = Sim::new(seed, &cfg);
        sim.settle().unwrap();
        let r = &sim.report;
        assert!(r.rescans <= 2 && r.deletes <= 1 && r.cancels <= 1, "{r:?}");
        assert_eq!(sim.rescans_left, 2 - r.rescans);
        assert_eq!(sim.deletes_left, 1 - r.deletes);
        assert_eq!(sim.cancels_left, 1 - r.cancels);
        (rescans, deletes) = (rescans + r.rescans, deletes + r.deletes);
    }
    assert!(rescans > 0 && deletes > 0);
}
