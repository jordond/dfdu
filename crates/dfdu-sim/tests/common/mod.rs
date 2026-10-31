//! Helpers the simulation scenario binaries share: the scripted config, finding nodes by path,
//! stepping workers to a point, running a script for each seed, and the commands they issue.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use dfdu_scan::{DirId, JobId};
use dfdu_sim::{Sim, SimConfig, Spec, quiet_injected_panics};
use dfdu_tree::{DirState, Ix};

/// Seeds for the random tail of each scenario.
pub const SEEDS: u64 = 20;

/// Three workers, blocks of 3 entries, chunk credit 2; outboxes large enough that nothing is
/// flushed unless a script flushes it, a worker goes idle, or the watchdog does; no random
/// commands.
pub fn cfg() -> SimConfig {
    let mut c = SimConfig {
        rescans: 0,
        deletes: 0,
        cancels: 0,
        ..SimConfig::default()
    };
    c.limits.outbox_msgs = 1000;
    c.limits.outbox_bytes = 1 << 20;
    c
}

/// The node at `path` below the root ("" is the root), by name.
pub fn find(sim: &Sim, path: &str) -> Ix {
    let t = &sim.model.tree;
    let mut ix = t.roots().next().expect("root");
    for part in path.split('/').filter(|p| !p.is_empty()) {
        ix = t
            .children(ix)
            .find(|&c| t.name(c) == part.as_bytes())
            .unwrap_or_else(|| panic!("{path}: no {part}"));
    }
    ix
}

/// The scan ids of the directory at `path`.
pub fn ids(sim: &Sim, path: &str) -> (JobId, DirId) {
    let d = sim.model.tree.dir(find(sim, path)).expect("directory");
    (d.job, d.dir_id)
}

pub fn state(sim: &Sim, path: &str) -> DirState {
    sim.model
        .tree
        .dir(find(sim, path))
        .expect("directory")
        .state
}

/// Steps worker `w` until `done` holds, at most 200 steps.
pub fn step_until(sim: &mut Sim, w: usize, mut done: impl FnMut(&Sim) -> bool) {
    for _ in 0..200 {
        if done(sim) {
            return;
        }
        sim.step_worker(w);
    }
    panic!("worker {w} did not get there in 200 steps");
}

/// Worker `w` is running part `part` of `(job, dir)`.
pub fn runs(sim: &Sim, w: usize, (job, dir): (JobId, DirId), part: u32) -> bool {
    sim.workers[w].slot.current() == Some((job, dir, part))
}

pub fn idle(sim: &Sim, w: usize) -> bool {
    sim.workers[w].slot.current().is_none()
}

/// Lists the root on worker 0 and applies everything, leaving worker 0 idle with the root's
/// subdirectory tasks published.
pub fn scan_root(sim: &mut Sim) {
    sim.record = true;
    step_until(sim, 0, |s| {
        s.model.tree.roots().next().is_some()
            && s.workers[0].slot.outbox.lock().unpublished() > 0
            && idle(s, 0)
    });
    sim.flush_worker(0);
    sim.apply_all().unwrap();
}

/// Runs the scripted part `script` for each seed, then the random tail with every final check.
#[expect(
    clippy::needless_pass_by_value,
    reason = "scenarios build their config inline at the call"
)]
pub fn for_seeds(spec: impl Fn() -> Spec, cfg: SimConfig, script: impl Fn(&mut Sim)) {
    quiet_injected_panics();
    for seed in 0..SEEDS {
        let mut sim = Sim::with_spec(seed, &cfg, spec());
        script(&mut sim);
        if let Err(e) = sim.run_to_end() {
            panic!("{e}");
        }
    }
}

/// Lists `w` on worker 0 until its listing has ended: blocks 0 and 1 handed to chunk tasks
/// (credit 2), blocks 2 and 3 stat'ed by the listing itself. Publishes the chunk tasks and
/// applies what worker 0 sent.
pub fn list_w(sim: &mut Sim) -> (JobId, DirId) {
    scan_root(sim);
    let w = ids(sim, "w");
    step_until(sim, 0, |s| runs(s, 0, w, 0));
    step_until(sim, 0, |s| idle(s, 0));
    assert_eq!(sim.workers[0].slot.outbox.lock().unpublished(), 2);
    sim.flush_worker(0);
    sim.apply_all().unwrap();
    w
}

/// Steps every worker, applies everything and runs the watchdog until no job runs and
/// nothing is in flight.
pub fn finish_jobs(sim: &mut Sim) {
    for _ in 0..10_000 {
        let (out, ab) = sim.shared.outstanding();
        if sim.model.status.running_jobs() == 0
            && out == ab
            && !sim.applier.has_work()
            && sim.queued().is_empty()
        {
            return;
        }
        for w in 0..sim.workers.len() {
            sim.step_worker(w);
        }
        sim.apply_all().unwrap();
        sim.applier_work().unwrap();
        sim.watchdog();
        sim.tick();
    }
    panic!("jobs did not finish");
}

pub fn node(sim: &Sim, path: &str) -> dfdu_tree::NodeRef {
    sim.model.tree.node_ref(find(sim, path))
}

pub fn rescan(sim: &mut Sim, path: &str) {
    let node = node(sim, path);
    sim.command(dfdu_engine::Command::Rescan { node }).unwrap();
}
