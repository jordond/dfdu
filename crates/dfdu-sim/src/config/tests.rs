use super::*;
use crate::Sim;
use crate::test_util::{find, quiet, settled};

/// The simulation's rules load, and classify an empty directory below the root by the rule
/// added for it.
#[test]
fn the_sim_rules_classify_empty_directories() {
    let rules = sim_rules();
    let empty = rules
        .index("sim.empty")
        .expect("the rule for empty directories");
    let sim = settled(&SimConfig {
        rules: Some(rules),
        ..quiet()
    });
    let t = &sim.model.tree;
    assert_eq!(t.class(find(&sim, "e")).map(|c| c.rule), Some(empty));
    assert_eq!(t.class(find(&sim, "d")), None, "not empty");
}

/// What a config leaves open is picked per seed, within its bounds; what it sets holds for
/// every seed.
#[test]
fn what_is_left_open_is_picked_per_seed() {
    let (mut crossed, mut not) = (false, false);
    for seed in 0..40 {
        let sim = Sim::new(seed, &quiet());
        (crossed, not) = (crossed || sim.cross_mounts, not || !sim.cross_mounts);
        let l = &sim.shared.limits;
        assert!((1..=2).contains(&l.rotational_parts) && (1..=3).contains(&l.network_parts));
        assert_eq!(l.pool_workers, quiet().workers);
    }
    assert!(crossed && not);
    let set = SimConfig {
        cross_mounts: Some(true),
        network: Some(false),
        mount_limits: Some((5, 6)),
        ..quiet()
    };
    for seed in 0..10 {
        let sim = Sim::new(seed, &set);
        assert!(sim.cross_mounts && !sim.network);
        let l = &sim.shared.limits;
        assert_eq!((l.rotational_parts, l.network_parts), (5, 6));
    }
}
