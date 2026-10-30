use std::collections::HashSet;

use dfdu_rules::RuleIx;
use dfdu_tree::{HostPath, NodeRef};

use super::*;
use crate::test_util::{find, quiet};
use crate::{Sim, SimConfig, SpecBuilder, sim_rules};

/// The pool's answer is decided by the path: none tracked without the guard, every answer
/// for some path with it, and a named process running for some paths only.
#[test]
fn the_pools_answer_follows_the_path() {
    let node = vec!["node".to_string()];
    let paths: Vec<Vec<u8>> = (0..60).map(|i| format!("/sim/p{i}").into_bytes()).collect();
    let mut tracked = HashSet::new();
    let mut running = 0;
    for p in &paths {
        assert_eq!(pool_answer(p, true, &node), pool_answer(p, true, &node));
        assert_eq!(pool_answer(p, false, &[]).0, Some(false));
        assert_eq!(pool_answer(p, true, &[]).1, None, "no process to run");
        let (t, r) = pool_answer(p, true, &node);
        tracked.insert(t);
        running += usize::from(r.as_deref() == Some("node"));
    }
    assert_eq!(tracked.len(), 3, "{tracked:?}");
    assert!(running > 0 && running < paths.len());
}

/// A request is answered for the node and rule it names.
#[test]
fn a_request_is_answered_for_what_it_names() {
    let path = HostPath::from(std::path::PathBuf::from("/sim/p"));
    let req = CheckReq::Guards {
        node: NodeRef::from_bits(7),
        rule: RuleIx(3),
        path: path.clone(),
        vcs: true,
        processes: Vec::new(),
    };
    let CheckMsg::Guards {
        node,
        rule,
        tracked,
        running,
    } = sim_check(&req)
    else {
        panic!("a guards answer");
    };
    assert_eq!((node, rule), (NodeRef::from_bits(7), RuleIx(3)));
    assert_eq!((tracked, running), pool_answer(path.as_bytes(), true, &[]));
    let req = CheckReq::NativeSize {
        rule: RuleIx(1),
        check: Vec::new(),
        command: Vec::new(),
    };
    assert_eq!(
        sim_check(&req),
        CheckMsg::NativeSize {
            rule: RuleIx(1),
            result: Ok(0)
        }
    );
}

/// `/sim/{p/{package.json, node_modules/}, e/}`: a match whose guard the pool checks, and one
/// decided by the listing alone.
fn project(checks: bool) -> Sim {
    let mut b = SpecBuilder::new();
    let p = b.dir(crate::ROOT, "p");
    b.file(p, "package.json", 10);
    b.dir(p, "node_modules");
    b.dir(crate::ROOT, "e");
    let cfg = SimConfig {
        rules: Some(sim_rules()),
        checks,
        ..quiet()
    };
    let mut sim = Sim::with_spec(1, &cfg, b.build());
    sim.settle().unwrap();
    sim
}

/// The classes and protection of a run match the reference classifier's; a node protected
/// that should not be is found.
#[test]
fn classes_are_checked_against_the_reference() {
    let rules = sim_rules();
    let mut sim = project(true);
    assert_eq!(check_classes(&sim.spec, &sim.model, &rules), Ok(2));
    let p = find(&sim, "p");
    sim.model.tree.set_protection(p, true, false);
    assert_eq!(
        check_classes(&sim.spec, &sim.model, &rules),
        Err("/sim/p: protected true".into())
    );
}

/// The guarded match is checked by the pool, with the flags its path gives; without the
/// pool it is not checked.
#[test]
fn guarded_matches_carry_the_pools_answer() {
    let rules = sim_rules();
    let sim = project(true);
    assert!(check_pool_flags(&sim.model, &rules).is_ok());
    let sim = project(false);
    let err = check_pool_flags(&sim.model, &rules).unwrap_err();
    let path = sim.model.tree.path(find(&sim, "p/node_modules"));
    assert!(err.ends_with(": not checked"), "{err}");
    assert!(err.starts_with(&*String::from_utf8_lossy(&path)), "{err}");
}
