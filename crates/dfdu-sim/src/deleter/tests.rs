use dfdu_engine::{Command, Mode};

use super::*;
use crate::test_util::{find, quiet, settled};

/// A delete of `paths` issued on a settled run of the small spec.
fn deleting(paths: &[&str]) -> Sim {
    let mut sim = settled(&quiet());
    let items = paths
        .iter()
        .map(|p| sim.model.tree.node_ref(find(&sim, p)))
        .collect();
    sim.command(Command::Delete {
        items,
        mode: Mode::Permanent,
    })
    .unwrap();
    sim
}

/// A scenario's results go to the running plan's items in turn, a deleted one leaving the
/// spec, then the plan ends; with no plan running there is nothing to do.
#[test]
fn a_scenario_deletes_item_by_item() {
    let mut sim = deleting(&["a", "d"]);
    let plan = sim.running_plan().unwrap();
    assert_eq!(plan.len(), 2);
    let a = sim.shared.backend.loc_from_path(&plan[0]);
    assert_eq!(sim.delete_next(DeleteResult::Deleted), Ok(true));
    assert!(sim.spec.is_deleted(a));
    assert_eq!(sim.delete_next(DeleteResult::Aborted), Ok(true));
    assert_eq!(sim.report.deleted_items, 1);
    assert_eq!(
        sim.delete_next(DeleteResult::Deleted),
        Ok(true),
        "the plan's end"
    );
    assert!(sim.running_plan().is_none());
    let s = sim.model.status.last_delete.clone().unwrap();
    assert_eq!((s.deleted, s.aborted), (1, 1));
    assert_eq!(sim.delete_next(DeleteResult::Deleted), Ok(false));
}

/// What is picked for a delete is one to three entries below the root, none twice, each one
/// the spec lets the deleter remove; nothing is picked from a tree without a root.
#[test]
fn deletes_pick_entries_the_spec_can_lose() {
    let mut sim = settled(&quiet());
    for _ in 0..50 {
        let picked = pick_deletable(&sim.model.tree, &sim.spec, &sim.shared, &mut sim.rng);
        assert!((1..=3).contains(&picked.len()), "{picked:?}");
        let mut ixs: Vec<_> = picked
            .iter()
            .map(|&r| sim.model.tree.resolve_visible(r).unwrap())
            .collect();
        for &ix in &ixs {
            assert!(sim.model.tree.parent(ix).is_some(), "not the root");
            let n = sim
                .shared
                .backend
                .loc_from_path(&sim.model.tree.path_buf(ix));
            assert!(sim.spec.deletable(n));
        }
        ixs.sort_unstable_by_key(|ix| ix.0);
        ixs.dedup();
        assert_eq!(ixs.len(), picked.len(), "none twice");
    }
    let empty = Tree::new();
    let none = pick_deletable(&empty, &sim.spec, &sim.shared, &mut sim.rng);
    assert!(none.is_empty(), "{none:?}");
}

/// The simulated deleter answers each item of the oldest plan, then its end, then nothing; a
/// deleted item leaves the spec and is counted.
#[test]
fn the_simulated_deleter_answers_every_item() {
    let mut sim = deleting(&["a", "d", "e"]);
    let mut plans = std::mem::take(&mut sim.plans);
    let (id, items) = (plans[0].0.id, plans[0].0.items.len());
    let mut report = SimReport::default();
    let mut results = Vec::new();
    while let Some(msg) = deleter_step(
        &sim.spec,
        &sim.shared,
        &mut plans,
        &mut sim.rng,
        &mut report,
    ) {
        match msg {
            DeleterMsg::Item { plan, result, .. } => {
                assert_eq!(plan, id);
                results.push(result);
            }
            DeleterMsg::Done { plan, .. } => assert_eq!(plan, id),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(results.len(), items);
    assert!(plans.is_empty());
    let deleted = results
        .iter()
        .filter(|r| **r == DeleteResult::Deleted)
        .count();
    assert_eq!(report.deleted_items, deleted as u64);
}
