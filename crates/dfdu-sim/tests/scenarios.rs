//! Scripted regression scenarios for the scan pipeline. Each test builds a small filesystem, forces
//! the scenario's sequence by stepping workers, the watchdog and the applier by hand, checks what
//! that sequence must lead to, then finishes the run in random order with every check of the random
//! simulation (invariants after each step, totals against the spec, no lost, corrupt or duplicate
//! messages). The random tail runs for several seeds, so what follows the scripted part varies too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use dfdu_scan::{ChunkStatus, DirId, JobId, ListingStatus, ScanMsg, StepResult};
use dfdu_sim::{Fault, Sim, SimConfig, Spec, SpecBuilder};
use dfdu_tree::DirState;

use common::{cfg, find, for_seeds, idle, ids, list_w, runs, scan_root, state, step_until};

fn is_dir_msg(m: &ScanMsg, id: (JobId, DirId)) -> bool {
    match m {
        ScanMsg::Chunk(c) => (c.job, c.dir) == id,
        ScanMsg::Close { job, dir, .. } | ScanMsg::Stalled { job, dir } => (*job, *dir) == id,
        ScanMsg::Quiescent { .. } => false,
    }
}

fn closes(sim: &Sim, id: (JobId, DirId)) -> Vec<ListingStatus> {
    sim.log
        .iter()
        .filter_map(|m| match m {
            ScanMsg::Close {
                job, dir, status, ..
            } if (*job, *dir) == id => Some(status.clone()),
            _ => None,
        })
        .collect()
}

/// A spec with the root holding one directory `w` of 12 files (four blocks).
fn wide(fault: Fault) -> Spec {
    let mut b = SpecBuilder::new();
    let w = b.dir(dfdu_sim::ROOT, "w");
    b.files(w, "f", 12);
    b.fault(w, fault);
    b.build()
}

/// A directory split into chunks, one chunk panicking, `Close` arriving before the last chunk:
/// the `Close` is held until the last chunk, then the listing closes as `Partial`.
#[test]
fn panicking_chunk_and_close_before_last_chunk() {
    for_seeds(
        || wide(Fault::StatPanic(1)),
        cfg(),
        |sim| {
            let w = list_w(sim);
            // Worker 1 stats block 0 and keeps the chunk in its outbox.
            step_until(sim, 1, |s| runs(s, 1, w, 1));
            step_until(sim, 1, |s| idle(s, 1));
            // Worker 2 panics stat'ing block 1, finishing the last part: it sends the Close.
            step_until(sim, 2, |s| runs(s, 2, w, 2));
            step_until(sim, 2, |s| idle(s, 2));
            sim.flush_worker(2);
            let q = sim.queued();
            assert!(
                matches!(&q[..], [ScanMsg::Chunk(c), ScanMsg::Close { chunks: 4, .. }]
                    if c.seq == 1 && matches!(c.status, ChunkStatus::Panicked(_))),
                "{q:?}"
            );
            sim.apply_all().unwrap();
            assert_eq!(state(sim, "w"), DirState::Pending, "Close held for chunk 0");
            assert!(!sim.model.tree.dir(find(sim, "w")).unwrap().listing_closed);
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            assert_eq!(state(sim, "w"), DirState::Partial);
            assert_eq!(
                closes(sim, w),
                [ListingStatus::Partial {
                    panicked: 1,
                    cancelled: 0,
                    read_error: None
                }]
            );
            assert_eq!(sim.model.tree.children(find(sim, "w")).len(), 9);
        },
    );
}

/// The same directory with one chunk panicking and the next cancelled: the job is cancelled
/// between the two chunk tasks. The last part still sends one `Close` counting both, and
/// every part is accounted for, so the scanner reaches quiescence.
#[test]
fn panicking_and_cancelled_chunks() {
    for_seeds(
        || wide(Fault::StatPanic(0)),
        cfg(),
        |sim| {
            let w = list_w(sim);
            step_until(sim, 1, |s| runs(s, 1, w, 1));
            step_until(sim, 1, |s| idle(s, 1));
            let job = sim.model.status.jobs[0].id;
            sim.command(dfdu_engine::Command::Cancel { job }).unwrap();
            // Worker 2 takes the chunk task for block 1 and turns it away.
            assert_eq!(sim.step_worker(2), StepResult::Progress);
            assert!(idle(sim, 2));
            sim.flush_worker(2);
            let q = sim.queued();
            assert!(
                matches!(&q[..], [ScanMsg::Chunk(c), ScanMsg::Close { chunks: 4, status, .. }]
                    if c.seq == 1 && c.status == ChunkStatus::Cancelled
                    && *status == ListingStatus::Partial { panicked: 1, cancelled: 1, read_error: None }),
                "{q:?}"
            );
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            sim.applier_work().unwrap();
            assert_eq!(sim.shared.outstanding(), (0, 0));
            assert_eq!(state(sim, "w"), DirState::Cancelled);
        },
    );
}

/// `w` with twelve entries: a subdirectory `s` in block 0 and `t` in block 1. While `t` has
/// not finished, `w` is not terminal and its job keeps running, so messages for `w` after an
/// abandon are counted late (not discarded with a finished job).
fn wide_with_subdirs(fault: Fault) -> Spec {
    let mut b = SpecBuilder::new();
    let w = b.dir(dfdu_sim::ROOT, "w");
    b.file(w, "a", 10);
    for (name, rest) in [("s", 1), ("t", 2)] {
        let d = b.dir(w, name);
        b.files(d, "g", 2);
        b.files(w, &format!("{name}f"), rest);
    }
    b.files(w, "f", 6);
    b.fault(w, fault);
    b.build()
}

/// `w`'s listing is closed as abandoned; it stays non-terminal until `t` reports.
fn abandoned_open(sim: &Sim) -> bool {
    let d = sim.model.tree.dir(find(sim, "w")).unwrap();
    d.listing_closed && d.outcome == DirState::Abandoned && d.state == DirState::Pending
}

/// Runs watchdog rounds (1 ms each) until something is delivered, at most `max`.
fn watchdog_until_delivery(sim: &mut Sim, max: usize) {
    for _ in 0..max {
        sim.watchdog();
        if !sim.queued().is_empty() {
            return;
        }
    }
    panic!("nothing delivered in {max} watchdog rounds");
}

/// A stall in one part of a multi-part directory, then abandon, then the stuck part returns.
/// The abandon closes the listing at once and the stuck part stops counting toward
/// quiescence; what it sends on returning is late, and the task for the subdirectory its
/// block names is stopped.
#[test]
fn stalled_part_abandoned_then_returns() {
    const STALL: u64 = 1_000_000_000;
    for_seeds(
        || {
            wide_with_subdirs(Fault::StatStall {
                block: 0,
                dur_ns: Some(STALL),
            })
        },
        cfg(),
        |sim| {
            let w = list_w(sim);
            step_until(sim, 1, |s| runs(s, 1, w, 1));
            assert_eq!(sim.step_worker(1), StepResult::Blocked);
            step_until(sim, 2, |s| runs(s, 2, w, 2));
            step_until(sim, 2, |s| idle(s, 2));
            // Block 1 names `t`; this flush publishes its task, which nobody runs yet.
            sim.flush_worker(2);
            sim.apply_all().unwrap();
            watchdog_until_delivery(sim, 20);
            assert!(
                matches!(&sim.queued()[..], [ScanMsg::Stalled { job, dir }] if (*job, *dir) == w)
            );
            sim.apply_all().unwrap();
            assert!(sim.model.tree.dir(find(sim, "w")).unwrap().stalled);

            let node = sim.model.tree.node_ref(find(sim, "w"));
            sim.command(dfdu_engine::Command::Abandon { node }).unwrap();
            assert!(abandoned_open(sim));
            assert_eq!(sim.shared.outstanding(), (2, 1), "the stuck part and t");

            // The stuck part returns: its chunk and the Close are late.
            sim.advance(STALL);
            step_until(sim, 1, |s| idle(s, 1));
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            let st = &sim.model.status.stats;
            assert_eq!((st.late, st.duplicate), (2, 0));
            assert!(abandoned_open(sim));
            assert_eq!(sim.model.tree.children(find(sim, "w")).len(), 9);
            // The task for `s` was published by that flush; it finds its stop bit. `t` runs
            // normally, and `w` becomes terminal when it reports.
            step_until(sim, 2, |s| {
                let ob = s.workers[2].slot.outbox.lock();
                let closes = ob
                    .messages()
                    .iter()
                    .filter(|m| matches!(m, ScanMsg::Close { .. }));
                idle(s, 2) && closes.count() == 2
            });
            assert!(ran_stopped_task(sim), "the task for s");
            sim.flush_worker(2);
            sim.apply_all().unwrap();
            assert_eq!(sim.shared.outstanding(), (0, 0));
            assert_eq!(state(sim, "w"), DirState::Abandoned);
        },
    );
}

/// Worker 2 holds a `Close` with no chunks and status `Cancelled`: a task that found its stop
/// bit ran there.
fn ran_stopped_task(sim: &Sim) -> bool {
    idle(sim, 2)
        && sim.workers[2]
            .slot
            .outbox
            .lock()
            .messages()
            .iter()
            .any(|m| {
                matches!(
                    m,
                    ScanMsg::Close {
                        status: ListingStatus::Cancelled,
                        chunks: 0,
                        ..
                    }
                )
            })
}

/// The root holds `p`, whose one block names a directory `b` whose read never returns, then
/// `a`. A worker runs the first child from its private stack first.
fn parent_with_stuck_child() -> Spec {
    let mut b = SpecBuilder::new();
    let p = b.dir(dfdu_sim::ROOT, "p");
    let st = b.dir(p, "b");
    b.files(st, "y", 2);
    let a = b.dir(p, "a");
    b.files(a, "x", 2);
    b.fault(
        st,
        Fault::Stall {
            block: 0,
            dur_ns: None,
        },
    );
    b.build()
}

/// The age flush after the stall limit, so only the stall can make the watchdog flush.
fn cfg_late_age_flush() -> SimConfig {
    let mut c = cfg();
    c.limits.flush_age_ns = 5 * c.limits.stall_ns;
    c
}

/// Worker 1 lists `p` and then runs `b` from its private stack until it blocks reading it,
/// with `p`'s chunk and `Close` and the task for `a` still in its outbox.
fn block_in_child(sim: &mut Sim) -> ((JobId, DirId), (JobId, DirId)) {
    scan_root(sim);
    let p = ids(sim, "p");
    step_until(sim, 1, |s| runs(s, 1, p, 0));
    step_until(sim, 1, |s| idle(s, 1));
    let b = (p.0, DirId(p.1.0 + 1));
    let mut blocked = false;
    for _ in 0..20 {
        if sim.step_worker(1) == StepResult::Blocked {
            blocked = true;
            break;
        }
    }
    assert!(blocked && runs(sim, 1, b, 0), "worker 1 blocked in b");
    let ob = sim.workers[1].slot.outbox.lock();
    assert_eq!(ob.unpublished(), 1, "the task for a");
    assert!(ob.messages().iter().all(|m| is_dir_msg(m, p)));
    drop(ob);
    assert!(sim.queued().is_empty());
    (p, b)
}

/// A worker blocked in a syscall while its outbox holds its parent's chunk and unpublished
/// child tasks: the watchdog delivers both, then `Stalled`, in that order and in one flush.
#[test]
fn watchdog_delivers_outbox_of_blocked_worker() {
    for_seeds(parent_with_stuck_child, cfg_late_age_flush(), |sim| {
        let (p, b) = block_in_child(sim);
        watchdog_until_delivery(sim, 20);
        let q = sim.queued();
        assert!(
            matches!(&q[..], [ScanMsg::Chunk(c), ScanMsg::Close { .. }, ScanMsg::Stalled { job, dir }]
                if (c.job, c.dir) == p && (*job, *dir) == b),
            "{q:?}"
        );
        // The task for `a` was published by the same flush.
        assert_eq!(sim.step_worker(2), StepResult::Progress);
        assert!(runs(sim, 2, (p.0, DirId(p.1.0 + 2)), 0));
        sim.apply_all().unwrap();
        assert!(sim.model.tree.dir(find(sim, "p/b")).unwrap().stalled);
    });
}

/// The same with the byte budget exhausted when the stall is detected. The flush fails, so
/// nothing is sent and the task stays unpublished; the `Stalled` waits in the outbox behind
/// the parent's messages. Once the applier frees budget, the next age flush delivers all of it
/// in order.
#[test]
fn stall_detected_with_budget_exhausted() {
    let cfg = cfg_late_age_flush();
    let (stall, age) = (cfg.limits.stall_ns, cfg.limits.flush_age_ns);
    for_seeds(parent_with_stuck_child, cfg.clone(), |sim| {
        let (p, b) = block_in_child(sim);
        let cap = sim.sink.budget.capacity();
        let held = sim.sink.budget.try_acquire(cap).expect("budget free");
        let start = sim.clock_now();
        while sim.clock_now() - start <= stall + 1_000_000 {
            sim.watchdog();
        }
        assert!(sim.queued().is_empty(), "nothing fits");
        {
            let ob = sim.workers[1].slot.outbox.lock();
            assert!(
                matches!(ob.messages().last(), Some(ScanMsg::Stalled { job, dir }) if (*job, *dir) == b)
            );
            assert_eq!(ob.unpublished(), 1);
        }
        assert_eq!(
            sim.step_worker(2),
            StepResult::Idle,
            "the task for a is not stealable"
        );
        sim.sink.budget.release(held);
        watchdog_until_delivery(sim, (age / 1_000_000) as usize + 2);
        let q = sim.queued();
        assert!(
            matches!(&q[..], [ScanMsg::Chunk(c), ScanMsg::Close { .. }, ScanMsg::Stalled { job, dir }]
                if (c.job, c.dir) == p && (*job, *dir) == b),
            "{q:?}"
        );
        assert_eq!(sim.step_worker(2), StepResult::Progress);
    });
}

/// `Close` for a directory arriving just before its `Stalled`, then an abandon. A chunk part
/// stalls and its `Stalled` cannot be sent (budget); the part returns, and the listing, which
/// finishes last, sends the `Close` from another worker first. The `Close` is held for the
/// chunk; the `Stalled` marks the directory; the abandon closes it; the chunk is late.
#[test]
fn close_before_stalled_then_abandon() {
    const STALL: u64 = 30_000_000;
    let mut cfg = cfg();
    cfg.limits.chunk_credit = 1;
    for_seeds(
        || {
            wide_with_subdirs(Fault::StatStall {
                block: 0,
                dur_ns: Some(STALL),
            })
        },
        cfg.clone(),
        |sim| {
            scan_root(sim);
            let w = ids(sim, "w");
            step_until(sim, 0, |s| {
                runs(s, 0, w, 0) && s.workers[0].slot.outbox.lock().unpublished() == 1
            });
            sim.flush_worker(0);
            step_until(sim, 1, |s| runs(s, 1, w, 1));
            assert_eq!(sim.step_worker(1), StepResult::Blocked);
            let cap = sim.sink.budget.capacity();
            let held = sim.sink.budget.try_acquire(cap).expect("budget free");
            while !sim.workers[1]
                .slot
                .outbox
                .lock()
                .messages()
                .iter()
                .any(|m| matches!(m, ScanMsg::Stalled { .. }))
            {
                sim.watchdog();
            }
            sim.sink.budget.release(held);
            // The listing stats blocks 1 and 2 itself (no credit left)...
            let seq2 =
                |s: &Sim| {
                    s.workers[0].slot.outbox.lock().messages().iter().any(
                        |m| matches!(m, ScanMsg::Chunk(c) if (c.job, c.dir) == w && c.seq == 2),
                    )
                };
            step_until(sim, 0, seq2);
            // ...the stalled part returns, and is not the last part...
            sim.advance(STALL);
            step_until(sim, 1, |s| idle(s, 1));
            // ...and the listing stats the last block itself (no block is handed off unless
            // more follow) and sends the Close.
            step_until(sim, 0, |s| idle(s, 0));
            sim.flush_worker(0);
            assert!(matches!(
                sim.queued().last(),
                Some(ScanMsg::Close { chunks: 4, .. })
            ));
            sim.apply_all().unwrap();
            assert_eq!(state(sim, "w"), DirState::Pending, "Close held");
            sim.flush_worker(1);
            assert!(
                matches!(&sim.queued()[..], [ScanMsg::Stalled { .. }, ScanMsg::Chunk(c)] if c.seq == 0)
            );
            sim.apply_next().unwrap();
            assert!(sim.model.tree.dir(find(sim, "w")).unwrap().stalled);
            let node = sim.model.tree.node_ref(find(sim, "w"));
            sim.command(dfdu_engine::Command::Abandon { node }).unwrap();
            assert!(abandoned_open(sim), "t is not done");
            sim.apply_all().unwrap();
            let st = &sim.model.status.stats;
            assert_eq!((st.late, st.duplicate, st.corrupt), (1, 0, 0));
            assert_eq!(closes(sim, w).len(), 1);
        },
    );
}

/// A worker running a child from its private stack whose chunk tasks would be stolen before
/// the parent's chunk is flushed. Impossible: until worker 0 flushes, nothing it created is
/// stealable, and its flush sends the chunk naming the child before publishing the child's
/// chunk tasks.
#[test]
fn chunk_tasks_of_unflushed_child_are_not_stealable() {
    for_seeds(
        || wide(Fault::None),
        cfg(),
        |sim| {
            sim.record = true;
            // Worker 0 lists the root, then `w` from its private stack, without flushing.
            for _ in 0..200 {
                // The root's listing leaves one task (w); w's listing two chunk tasks.
                if sim.workers[0].slot.outbox.lock().unpublished() == 2 && idle(sim, 0) {
                    break;
                }
                sim.step_worker(0);
                assert_eq!(sim.step_worker(1), StepResult::Idle);
                assert_eq!(sim.step_worker(2), StepResult::Idle);
            }
            assert!(sim.queued().is_empty());
            assert_eq!(sim.model.status.stats.messages, 0);
            sim.flush_worker(0);
            // The root's messages, naming `w`, come before anything about `w`.
            let q = sim.queued();
            let first_w = q
                .iter()
                .position(|m| matches!(m, ScanMsg::Chunk(c) if c.dir != DirId(0)))
                .expect("a chunk of w");
            assert!(
                q[..first_w]
                    .iter()
                    .any(|m| matches!(m, ScanMsg::Chunk(c) if !c.subdirs.is_empty()))
            );
            // Now the chunk tasks can be stolen; their messages follow worker 0's.
            assert_eq!(sim.step_worker(1), StepResult::Progress);
        },
    );
}

/// Quiescence raced by a worker that steals a task right after another publishes and parks:
/// no watchdog round sends `Quiescent` while a task exists, published or running. And a
/// `Quiescent` already sent before a rescan job started does not judge that job.
#[test]
fn quiescence_with_published_and_new_tasks() {
    for_seeds(
        || {
            let mut b = SpecBuilder::new();
            let p = b.dir(dfdu_sim::ROOT, "p");
            for d in ["a", "b", "c"] {
                let x = b.dir(p, d);
                b.files(x, "f", 4);
            }
            b.build()
        },
        cfg(),
        |sim| {
            scan_root(sim);
            let quiescent = |s: &Sim| {
                s.queued()
                    .iter()
                    .any(|m| matches!(m, ScanMsg::Quiescent { .. }))
            };
            // Worker 1 lists `p`, publishes its children and parks (is not stepped).
            step_until(sim, 1, |s| !idle(s, 1));
            step_until(sim, 1, |s| idle(s, 1));
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            sim.watchdog();
            assert!(!quiescent(sim), "published tasks are outstanding");
            // Worker 2 steals one at once; rounds while it runs.
            for _ in 0..30 {
                sim.step_worker(2);
                if sim.shared.outstanding().0 == 0 {
                    break;
                }
                sim.watchdog();
                assert!(!quiescent(sim));
                sim.apply_all().unwrap();
                sim.step_worker(0);
            }
            // Finish the scan; the watchdog then reports quiescence, and it is not applied yet.
            for _ in 0..200 {
                if sim.model.status.running() == 0 && sim.shared.outstanding().0 == 0 {
                    break;
                }
                for w in 0..3 {
                    sim.step_worker(w);
                }
                sim.apply_all().unwrap();
                sim.applier_work().unwrap();
            }
            assert_eq!(sim.model.status.running(), 0);
            sim.watchdog();
            assert!(quiescent(sim));
            // A rescan job starts before that Quiescent is applied.
            let node = sim.model.tree.node_ref(find(sim, "p"));
            sim.command(dfdu_engine::Command::Rescan { node }).unwrap();
            sim.apply_all().unwrap();
            assert_eq!(sim.model.status.stats.lost, 0);
            assert_eq!(sim.model.status.running(), 1, "the rescan is still running");
        },
    );
}
