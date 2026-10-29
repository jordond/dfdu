//! Scripted scenarios for rule classification: markers a directory's class depends on in another
//! block of its parent, and late classes or classifier answers for a slot that was freed and
//! reused. Each forces its sequence by hand, checks what it must lead to, then finishes the run in
//! random order with every check of the random simulation, the classes checked against a
//! reference, for several seeds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use dfdu_scan::{DirId, JobId, ScanMsg};
use dfdu_sim::{Sim, SimConfig, Spec, SpecBuilder};
use dfdu_tree::{Class, Safety};

use common::{
    cfg, find, finish_jobs, for_seeds, idle, ids, list_w, rescan, runs, scan_root, step_until,
};

/// [`cfg`] with the built-in rules, every class checked against a reference at the end.
fn cfg_rules() -> SimConfig {
    SimConfig {
        rules: Some(dfdu_sim::sim_rules()),
        ..cfg()
    }
}

/// The root holds `w`, a Cargo project with a `node_modules`, twelve entries in four blocks:
/// `target` and `node_modules` in block 0, `Cargo.toml`, `package.json` and `yarn.lock` in
/// block 3. `target` has a CACHEDIR.TAG.
fn project_in_blocks() -> Spec {
    let mut b = SpecBuilder::new();
    let w = b.dir(dfdu_sim::ROOT, "w");
    let t = b.dir(w, "target");
    let tag = b.file(t, "CACHEDIR.TAG", 50);
    b.content(tag, dfdu_sim::CACHEDIR_TAG);
    b.file(t, "x", 10);
    let n = b.dir(w, "node_modules");
    b.file(n, "y", 10);
    b.files(w, "a", 7);
    for name in ["Cargo.toml", "package.json", "yarn.lock"] {
        b.file(w, name, 10);
    }
    b.build()
}

/// The classes the applied `Close` of `id` carried.
fn close_classes(sim: &Sim, id: (JobId, DirId)) -> Vec<(DirId, Class)> {
    sim.log
        .iter()
        .filter_map(|m| match m {
            ScanMsg::Close {
                job, dir, classes, ..
            } if (*job, *dir) == id => Some(classes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn rule_of(sim: &Sim, path: &str) -> Option<(String, Safety)> {
    let c = sim.model.tree.class(find(sim, path))?;
    let rules = sim.model.rules.as_ref().expect("rules");
    Some((rules.rule(c.rule).id.clone(), c.safety))
}

/// Lists `w` on worker 0 (blocks 0 and 1 to chunk tasks, 2 and 3 itself), then worker 1 stats
/// block 0, which names `target` and `node_modules`, and stops before running them.
fn list_project(sim: &mut Sim) -> (JobId, DirId) {
    let w = list_w(sim);
    step_until(sim, 1, |s| runs(s, 1, w, 1));
    step_until(sim, 1, |s| idle(s, 1));
    assert_eq!(sim.workers[1].slot.outbox.lock().unpublished(), 2);
    w
}

/// Classification across blocks, the subdirectories first: `target` and `node_modules` are read to the end
/// before `w`'s last block, which holds the siblings they need. Their `Close`s cannot decide,
/// and the last part of `w` does: its `Close` carries both classes.
#[test]
fn markers_in_other_chunks_subdirectories_first() {
    for_seeds(project_in_blocks, cfg_rules(), |sim| {
        let w = list_project(sim);
        let (t, n) = ((w.0, DirId(w.1.0 + 1)), (w.0, DirId(w.1.0 + 2)));
        step_until(sim, 1, |s| {
            s.workers[1]
                .slot
                .outbox
                .lock()
                .messages()
                .iter()
                .filter(|m| matches!(m, ScanMsg::Close { dir, .. } if *dir == t.1 || *dir == n.1))
                .count()
                == 2
        });
        // Worker 2 stats block 1, the last part of `w`.
        step_until(sim, 2, |s| runs(s, 2, w, 2));
        step_until(sim, 2, |s| idle(s, 2));
        sim.flush_worker(1);
        sim.flush_worker(2);
        sim.apply_all().unwrap();
        assert!(close_classes(sim, t).is_empty() && close_classes(sim, n).is_empty());
        let got: Vec<DirId> = close_classes(sim, w).iter().map(|c| c.0).collect();
        assert_eq!(got, [t.1, n.1]);
        assert_eq!(
            rule_of(sim, "w/target"),
            Some(("rust.target".into(), Safety::Regenerable))
        );
        assert_eq!(
            rule_of(sim, "w/node_modules"),
            Some(("node.node_modules".into(), Safety::Regenerable))
        );
    });
}

/// Classification across blocks, the parent first: `w`'s listing ends before `target` and `node_modules` are
/// read, so their own `Close`s decide.
#[test]
fn markers_in_other_chunks_parent_first() {
    for_seeds(project_in_blocks, cfg_rules(), |sim| {
        let w = list_project(sim);
        let (t, n) = ((w.0, DirId(w.1.0 + 1)), (w.0, DirId(w.1.0 + 2)));
        step_until(sim, 2, |s| runs(s, 2, w, 2));
        step_until(sim, 2, |s| idle(s, 2));
        step_until(sim, 1, |s| {
            s.workers[1]
                .slot
                .outbox
                .lock()
                .messages()
                .iter()
                .filter(|m| matches!(m, ScanMsg::Close { dir, .. } if *dir == t.1 || *dir == n.1))
                .count()
                == 2
        });
        sim.flush_worker(2);
        sim.flush_worker(1);
        sim.apply_all().unwrap();
        assert_eq!(
            close_classes(sim, w),
            [] as [(dfdu_scan::DirId, dfdu_tree::Class); 0]
        );
        assert_eq!(close_classes(sim, t).len(), 1);
        assert_eq!(close_classes(sim, n).len(), 1);
        assert_eq!(
            rule_of(sim, "w/target"),
            Some(("rust.target".into(), Safety::Regenerable))
        );
        assert_eq!(
            rule_of(sim, "w/node_modules"),
            Some(("node.node_modules".into(), Safety::Regenerable))
        );
    });
}

/// `p` holds a Cargo project's `target`, with CACHEDIR.TAG, and `Cargo.toml`.
fn small_project() -> Spec {
    let mut b = SpecBuilder::new();
    let p = b.dir(dfdu_sim::ROOT, "p");
    let t = b.dir(p, "target");
    let tag = b.file(t, "CACHEDIR.TAG", 50);
    b.content(tag, dfdu_sim::CACHEDIR_TAG);
    b.files(t, "o", 4);
    b.file(p, "Cargo.toml", 10);
    b.files(p, "s", 4);
    b.build()
}

/// A late class for a freed slot: the `Close` carrying `target`'s class is held back while `Cargo.toml` is
/// deleted, `p` is rescanned twice and the slots of the first `p` are freed and reused. The
/// class arrives for a slot that now holds another node, and must not be applied to it.
#[test]
fn class_for_a_freed_and_reused_slot() {
    for_seeds(small_project, cfg_rules(), |sim| {
        scan_root(sim);
        let p = ids(sim, "p");
        let t = (p.0, DirId(p.1.0 + 1));
        step_until(sim, 1, |s| runs(s, 1, p, 0));
        step_until(sim, 1, |s| {
            s.workers[1].slot.outbox.lock().messages().iter().any(|m| {
                matches!(m, ScanMsg::Close { dir, classes, .. } if *dir == t.1 && !classes.is_empty())
            })
        });
        sim.flush_worker(1);
        let held = sim
            .take_queued(|m| matches!(m, ScanMsg::Close { dir, .. } if *dir == t.1))
            .expect("target's Close");
        sim.apply_all().unwrap();
        let old = find(sim, "p/target");
        let cargo = sim
            .spec
            .live_children(1)
            .find(|&c| sim.spec.nodes[c as usize].name == b"Cargo.toml")
            .unwrap();
        sim.spec.delete_subtree(cargo);
        for _ in 0..2 {
            rescan(sim, "p");
            finish_jobs(sim);
        }
        // The slot holds a node of the last rescan.
        let t = &sim.model.tree;
        assert!(t.resolve(t.node_ref(old)).is_some(), "slot reused");
        sim.deliver(held);
        sim.apply_all().unwrap();
        let rules = sim.model.rules.clone().unwrap();
        assert!(
            sim.model
                .tree
                .classes()
                .all(|(_, c)| rules.rule(c.rule).id != "rust.target"),
            "a class for the freed slot was applied"
        );
    });
}

/// A late class for a freed slot, from the classifier pool: its answer for `target` (tracked by git, so protected)
/// is held back while `Cargo.toml` is deleted and `p` is rescanned twice, so the slot `target`
/// had holds another node when the answer arrives. The answer names the old node and is
/// dropped.
#[test]
fn classifier_answer_for_a_freed_and_reused_slot() {
    for_seeds(small_project, cfg_rules(), |sim| {
        sim.applier.checks = true;
        finish_jobs(sim);
        let reqs = sim.applier.take_check_requests();
        let old = find(sim, "p/target");
        let held: Vec<_> = dfdu_engine::check_batch(&reqs)
            .into_iter()
            .map(|m| match m {
                dfdu_engine::CheckMsg::Guards { node, rule, .. } => dfdu_engine::CheckMsg::Guards {
                    node,
                    rule,
                    tracked: Some(true),
                    running: None,
                },
                other @ dfdu_engine::CheckMsg::NativeSize { .. } => other,
            })
            .collect();
        assert_eq!(held.len(), 1, "one guard check, for target");
        let cargo = sim
            .spec
            .live_children(1)
            .find(|&c| sim.spec.nodes[c as usize].name == b"Cargo.toml")
            .unwrap();
        sim.spec.delete_subtree(cargo);
        for _ in 0..2 {
            rescan(sim, "p");
            finish_jobs(sim);
        }
        let t = &sim.model.tree;
        assert!(t.resolve(t.node_ref(old)).is_some(), "slot reused");
        for m in held {
            sim.applier.apply_check(&mut sim.model, m);
        }
        assert!(
            sim.model
                .tree
                .classes()
                .all(|(_, c)| !c.has(Class::VCS_TRACKED)),
            "an answer for the freed slot was applied"
        );
        sim.applier.take_check_requests();
        sim.applier.checks = false;
    });
}
