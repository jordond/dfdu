use dfdu_scan::ScanMsg;

use super::*;
use crate::test_util::{find, quiet, settled, small};

/// A simulation starts with the scan of the root issued and nothing delivered yet.
#[test]
fn a_simulation_starts_with_the_scan_issued() {
    let mut sim = Sim::with_spec(3, &quiet(), small());
    assert_eq!(sim.model.status.commands_applied, 1);
    assert_eq!(sim.model.status.jobs.len(), 1);
    assert!(sim.model.tree.roots().next().is_some());
    assert!(sim.queued().is_empty());
    assert!(matches!(sim.apply_next(), Ok(None)));
    assert_eq!(sim.clock_now(), 1);
    sim.advance(5);
    assert_eq!(sim.clock_now(), 6);
}

/// A message delivered by hand is queued for the applier, and can be taken out again.
#[test]
fn delivered_messages_are_queued_until_taken() {
    let mut sim = Sim::with_spec(3, &quiet(), small());
    sim.deliver(ScanMsg::Quiescent { submitted_seq: 0 });
    assert!(matches!(sim.queued()[..], [ScanMsg::Quiescent { .. }]));
    let quiescent = |m: &ScanMsg| matches!(m, ScanMsg::Quiescent { .. });
    assert!(sim.take_queued(quiescent).is_some());
    assert!(sim.take_queued(quiescent).is_none());
    assert!(sim.queued().is_empty());
}

/// Stepping a worker lists the root; once flushed and applied, its entries are in the tree.
#[test]
fn a_worker_step_lists_the_root() {
    let mut sim = Sim::with_spec(3, &quiet(), small());
    for _ in 0..20 {
        sim.step_worker(0);
    }
    sim.flush_worker(0);
    sim.apply_all().unwrap();
    sim.applier_work().unwrap();
    let t = &sim.model.tree;
    let root = t.roots().next().unwrap();
    let mut names: Vec<&[u8]> = t.children(root).map(|c| t.name(c)).collect();
    names.sort_unstable();
    assert_eq!(names, [&b"a"[..], b"d", b"e"]);
    sim.check_mount_limits().unwrap();
}

/// A check that fails after a command names the seed and how far the run got.
#[test]
fn a_failed_check_names_the_seed_and_the_ticks() {
    let mut sim = settled(&quiet());
    let ticks = sim.report.ticks;
    sim.shared.backend.wrong_storage.store(2, SeqCst);
    let node = sim.model.tree.node_ref(find(&sim, "d"));
    let e = sim.command(Command::Rescan { node }).unwrap_err();
    assert_eq!((e.seed, e.ticks), (1, Some(ticks)));
    assert_eq!(
        e.what,
        "2 directories opened with the storage of another mount after a command"
    );
}

/// Once the scan's jobs end, the measurements they asked for are answered, and those the
/// answers lead to, until none is left: generated specs share extents, so some are measured.
#[test]
fn measurements_are_answered_until_none_is_left() {
    let mut measured = 0;
    for seed in 0..10 {
        let mut sim = Sim::new(seed, &quiet());
        for _ in 0..10_000 {
            let (out, ab) = sim.shared.outstanding();
            if sim.model.status.running_jobs() == 0 && out == ab && !sim.applier.has_work() {
                break;
            }
            for w in 0..sim.workers.len() {
                sim.step_worker(w);
                sim.flush_worker(w);
            }
            sim.apply_all().unwrap();
            sim.applier_work().unwrap();
            sim.watchdog();
            sim.tick();
        }
        measured += sim.answer_measurements().unwrap();
        assert!(sim.meas_q.is_empty());
        assert!(sim.applier.take_measure_requests().is_empty());
    }
    assert!(measured > 0);
}

/// [`run`] is a simulation of the seed's spec from start to end.
#[test]
fn a_run_is_its_seeds_simulation() {
    let cfg = quiet();
    let a = run(5, &cfg).unwrap();
    let b = Sim::new(5, &cfg).run_to_end().unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert!(a.dirs > 0);
}
