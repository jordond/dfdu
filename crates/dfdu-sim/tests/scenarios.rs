//! Named regression scenarios (`docs/data-pipeline.md` §10.2), one test each. Each builds a
//! small filesystem, forces the scenario's sequence by stepping workers, the watchdog and the
//! applier by hand, checks what that sequence must lead to, then finishes the run in random
//! order with every check of the random simulation (invariants after each step, totals
//! against the spec, no lost, corrupt or duplicate messages). The random tail runs for
//! several seeds, so what follows the scripted part varies too.

use dfdu_scan::{ChunkStatus, DirId, JobId, ListingStatus, ScanMsg, StepResult};
use dfdu_sim::{Fault, Sim, SimConfig, Spec, SpecBuilder};
use dfdu_tree::{DirState, Ix};

/// Seeds for the random tail of each scenario.
const SEEDS: u64 = 20;

/// Three workers, blocks of 3 entries, chunk credit 2; outboxes large enough that nothing is
/// flushed unless a script flushes it, a worker goes idle, or the watchdog does; no random
/// commands.
fn cfg() -> SimConfig {
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

fn quiet_injected_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload().downcast_ref::<&str>().copied().unwrap_or("");
        if msg != "injected panic" {
            default(info);
        }
    }));
}

/// The node at `path` below the root ("" is the root), by name.
fn find(sim: &Sim, path: &str) -> Ix {
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
fn ids(sim: &Sim, path: &str) -> (JobId, DirId) {
    let d = sim.model.tree.dir(find(sim, path)).expect("directory");
    (JobId(d.job), DirId(d.dir_id))
}

fn state(sim: &Sim, path: &str) -> DirState {
    sim.model
        .tree
        .dir(find(sim, path))
        .expect("directory")
        .state
}

/// Steps worker `w` until `done` holds, at most 200 steps.
fn step_until(sim: &mut Sim, w: usize, mut done: impl FnMut(&Sim) -> bool) {
    for _ in 0..200 {
        if done(sim) {
            return;
        }
        sim.step_worker(w);
    }
    panic!("worker {w} did not get there in 200 steps");
}

/// Worker `w` is running part `part` of `(job, dir)`.
fn runs(sim: &Sim, w: usize, (job, dir): (JobId, DirId), part: u32) -> bool {
    sim.workers[w].slot.current() == Some((job, dir, part))
}

fn idle(sim: &Sim, w: usize) -> bool {
    sim.workers[w].slot.current().is_none()
}

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

/// Lists the root on worker 0 and applies everything, leaving worker 0 idle with the root's
/// subdirectory tasks published.
fn scan_root(sim: &mut Sim) {
    sim.record = true;
    step_until(sim, 0, |s| {
        s.model.tree.roots().next().is_some()
            && s.workers[0].slot.outbox.lock().unpublished() > 0
            && idle(s, 0)
    });
    sim.flush_worker(0);
    sim.apply_all().unwrap();
}

/// A spec with the root holding one directory `w` of 12 files (four blocks).
fn wide(fault: Fault) -> Spec {
    let mut b = SpecBuilder::new();
    let w = b.dir(dfdu_sim::ROOT, "w");
    b.files(w, "f", 12);
    b.fault(w, fault);
    b.build()
}

/// Runs the scripted part `script` for each seed, then the random tail with every final check.
fn for_seeds(spec: impl Fn() -> Spec, cfg: SimConfig, script: impl Fn(&mut Sim)) {
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
fn list_w(sim: &mut Sim) -> (JobId, DirId) {
    scan_root(sim);
    let w = ids(sim, "w");
    step_until(sim, 0, |s| runs(s, 0, w, 0));
    step_until(sim, 0, |s| idle(s, 0));
    assert_eq!(sim.workers[0].slot.outbox.lock().unpublished(), 2);
    sim.flush_worker(0);
    sim.apply_all().unwrap();
    w
}

// ---- 1 -----------------------------------------------------------------------------------

/// A directory split into chunks, one chunk panicking, `Close` arriving before the last chunk:
/// the `Close` is held until the last chunk, then the listing closes as `Partial`.
#[test]
fn s1_panicking_chunk_and_close_before_last_chunk() {
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
fn s1_panicking_and_cancelled_chunks() {
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

// ---- 2 -----------------------------------------------------------------------------------

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
fn s2_stalled_part_abandoned_then_returns() {
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

// ---- 3 and 4 -----------------------------------------------------------------------------

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
fn s3_watchdog_delivers_outbox_of_blocked_worker() {
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
fn s4_stall_detected_with_budget_exhausted() {
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

// ---- 5 -----------------------------------------------------------------------------------

/// `Close` for a directory arriving just before its `Stalled`, then an abandon. A chunk part
/// stalls and its `Stalled` cannot be sent (budget); the part returns, and the listing, which
/// finishes last, sends the `Close` from another worker first. The `Close` is held for the
/// chunk; the `Stalled` marks the directory; the abandon closes it; the chunk is late.
#[test]
fn s5_close_before_stalled_then_abandon() {
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

// ---- 6 -----------------------------------------------------------------------------------

/// A worker running a child from its private stack whose chunk tasks would be stolen before
/// the parent's chunk is flushed. Impossible: until worker 0 flushes, nothing it created is
/// stealable, and its flush sends the chunk naming the child before publishing the child's
/// chunk tasks.
#[test]
fn s6_chunk_tasks_of_unflushed_child_are_not_stealable() {
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

// ---- 7 -----------------------------------------------------------------------------------

/// Quiescence raced by a worker that steals a task right after another publishes and parks:
/// no watchdog round sends `Quiescent` while a task exists, published or running. And a
/// `Quiescent` already sent before a rescan job started does not judge that job.
#[test]
fn s7_quiescence_with_published_and_new_tasks() {
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

// ---- commands ----------------------------------------------------------------------------

/// Steps every worker, applies everything and runs the watchdog until no job runs and
/// nothing is in flight.
fn finish_jobs(sim: &mut Sim) {
    for _ in 0..10_000 {
        let (out, ab) = sim.shared.outstanding();
        if sim.model.status.running() == 0
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

fn rescans_running(sim: &Sim) -> usize {
    sim.model
        .status
        .jobs
        .iter()
        .filter(|j| {
            j.kind == dfdu_engine::JobKind::Rescan
                && matches!(
                    j.phase,
                    dfdu_engine::JobPhase::Running | dfdu_engine::JobPhase::Finishing
                )
        })
        .count()
}

fn last_notice(sim: &Sim) -> &str {
    sim.model.status.notices.last().map_or("", |s| s.as_str())
}

fn node(sim: &Sim, path: &str) -> dfdu_tree::NodeRef {
    sim.model.tree.node_ref(find(sim, path))
}

fn rescan(sim: &mut Sim, path: &str) {
    let node = node(sim, path);
    sim.command(dfdu_engine::Command::Rescan { node }).unwrap();
}

fn delete(sim: &mut Sim, paths: &[&str]) {
    let items = paths.iter().map(|p| node(sim, p)).collect();
    sim.command(dfdu_engine::Command::Delete {
        items,
        mode: dfdu_clean::Mode::Permanent,
    })
    .unwrap();
}

/// `/a/b/c` with two files at each level, and `/d` with two files.
fn nested() -> Spec {
    let mut b = SpecBuilder::new();
    let mut cur = dfdu_sim::ROOT;
    for name in ["a", "b", "c"] {
        cur = b.dir(cur, name);
        b.files(cur, &format!("{name}f"), 2);
    }
    let d = b.dir(dfdu_sim::ROOT, "d");
    b.files(d, "df", 2);
    b.build()
}

/// The delete rows of the overlap table (§7.2) against rescans: a rescan of the item, an
/// ancestor or a descendant of the running or a queued plan is rejected, including a rescan
/// requested while a delete in its subtree runs; once the plans are done, it is accepted.
#[test]
fn s8_rescan_while_delete_overlaps() {
    for_seeds(nested, cfg(), |sim| {
        finish_jobs(sim);
        delete(sim, &["a/b"]);
        assert_eq!(sim.running_plan().map(|p| p.len()), Some(1));
        for target in ["a", "a/b", "a/b/c"] {
            rescan(sim, target);
            assert!(
                last_notice(sim).contains("delete in this directory"),
                "{target}: {}",
                last_notice(sim)
            );
            assert_eq!(rescans_running(sim), 0);
        }
        // A second plan is queued behind the first; a rescan of its item is rejected too.
        delete(sim, &["d"]);
        assert!(sim.running_plan().unwrap()[0].ends_with("b"));
        rescan(sim, "d");
        assert!(
            last_notice(sim).contains("delete in this directory"),
            "{}",
            last_notice(sim)
        );
        assert_eq!(rescans_running(sim), 0);
        // The first plan deletes `b`; the queued plan starts.
        assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
        assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
        sim.applier_work().unwrap();
        assert!(sim.running_plan().unwrap()[0].ends_with("d"));
        rescan(sim, "a");
        assert_eq!(rescans_running(sim), 1, "a is no longer in a plan");
        assert!(
            sim.delete_next(dfdu_clean::DeleteResult::Stale("changed".into()))
                .unwrap()
        );
        assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
    });
}

/// The rescan rows against other rescans, and the delete rows against rescans: a delete of
/// the target, an ancestor or a descendant of a running rescan is refused; a rescan of the
/// target or a descendant is rejected; a rescan of an ancestor cancels the running one; a
/// rescan of a detached directory is rejected.
#[test]
fn s8_rescans_and_deletes_against_rescans() {
    for_seeds(nested, cfg(), |sim| {
        finish_jobs(sim);
        rescan(sim, "a/b");
        assert_eq!(rescans_running(sim), 1);
        for target in ["a", "a/b", "a/b/c"] {
            delete(sim, &[target]);
            assert!(
                last_notice(sim).contains("being rescanned"),
                "{target}: {}",
                last_notice(sim)
            );
            assert!(sim.running_plan().is_none());
        }
        for target in ["a/b", "a/b/c"] {
            rescan(sim, target);
            assert!(
                last_notice(sim).contains("already rescanning"),
                "{target}: {}",
                last_notice(sim)
            );
        }
        let old_a = node(sim, "a");
        rescan(sim, "a");
        assert_eq!(rescans_running(sim), 1, "the rescan of a/b was cancelled");
        finish_jobs(sim);
        assert!(sim.model.tree.resolve(old_a).is_none(), "swapped out");
        sim.command(dfdu_engine::Command::Rescan { node: old_a })
            .unwrap();
        assert!(last_notice(sim).contains("gone"), "{}", last_notice(sim));
        assert_eq!(rescans_running(sim), 0);
    });
}

/// The rows for a directory the initial scan is still reading: a rescan cuts it, and it
/// reports `Superseded` to its parent; a delete cuts it, and it reports `Deleting`.
#[test]
fn s8_rescan_and_delete_inside_running_scan() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        assert_eq!(state(sim, "a"), DirState::Pending);
        rescan(sim, "a");
        assert_eq!(state(sim, "a"), DirState::Superseded);
        assert_eq!(rescans_running(sim), 1);
        delete(sim, &["d"]);
        assert_eq!(state(sim, "d"), DirState::Deleting);
        assert!(sim.running_plan().is_some());
    });
}

// ---- 9 -----------------------------------------------------------------------------------

/// A delete of a directory still being scanned, then late messages for its descendants:
/// those that arrive while it is cut and those that arrive after it was detached are
/// discarded, and none counts as corrupt or makes anything lost. Worker 0 holds `keep` open,
/// so the job is still running when the late messages arrive.
#[test]
fn s9_delete_while_scanning_then_late_messages() {
    for_seeds(
        || {
            let mut b = SpecBuilder::new();
            let keep = b.dir(dfdu_sim::ROOT, "keep");
            b.files(keep, "k", 2);
            let d = b.dir(dfdu_sim::ROOT, "d");
            for name in ["e", "f"] {
                let x = b.dir(d, name);
                b.files(x, "x", 2);
            }
            b.file(d, "g", 5);
            b.build()
        },
        cfg(),
        |sim| {
            scan_root(sim);
            // Worker 1 lists `d`, then `e` from its private stack, sending nothing.
            let d = ids(sim, "d");
            step_until(sim, 1, |s| runs(s, 1, d, 0));
            step_until(sim, 0, |s| runs(s, 0, ids(s, "keep"), 0));
            step_until(sim, 1, |s| idle(s, 1));
            step_until(sim, 1, |s| runs(s, 1, (d.0, DirId(d.1.0 + 1)), 0));
            step_until(sim, 1, |s| idle(s, 1));
            assert!(sim.queued().is_empty());
            delete(sim, &["d"]);
            assert_eq!(state(sim, "d"), DirState::Deleting);
            // `d`'s chunk naming `e` and `f`, its Close and `e`'s messages: under the cut.
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            let cut = sim.model.status.stats.discarded_cut;
            assert!(cut >= 4, "{cut}");
            // After the detach, `f`'s task (published by that flush) finds its stop bit; its
            // Close is discarded too.
            assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
            sim.applier_work().unwrap();
            step_until(sim, 2, |s| {
                s.workers[2].slot.outbox.lock().messages().iter().any(|m| {
                    matches!(
                        m,
                        ScanMsg::Close {
                            status: ListingStatus::Cancelled,
                            ..
                        }
                    )
                })
            });
            sim.flush_worker(2);
            sim.apply_all().unwrap();
            let st = &sim.model.status.stats;
            assert!(st.discarded_cut > cut && st.discarded_job == 0, "{st:?}");
            assert_eq!((st.corrupt, st.lost, st.duplicate), (0, 0, 0));
            assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
        },
    );
}

// ---- 11 ----------------------------------------------------------------------------------

/// Number of charged links among the nodes at `paths`.
fn charged(sim: &Sim, paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            sim.model
                .tree
                .flags(find(sim, p))
                .has(dfdu_tree::NodeFlags::CHARGED)
        })
        .map(|p| p.to_string())
        .collect()
}

/// A hard-linked inode with links inside and outside a deleted subtree, then inside and
/// outside a rescanned subtree. The charge moves to the lowest remaining path after the
/// delete, and stays single after the rescan's scope is merged.
#[test]
fn s11_hard_links_across_delete_and_rescan() {
    for_seeds(
        || {
            let mut b = SpecBuilder::new();
            let x = b.dir(dfdu_sim::ROOT, "x");
            let f = b.file(x, "f", 50_000);
            let y = b.dir(dfdu_sim::ROOT, "y");
            b.link(y, "h", f);
            b.files(y, "o", 2);
            let z = b.dir(dfdu_sim::ROOT, "z");
            b.link(z, "k", f);
            b.build()
        },
        cfg(),
        |sim| {
            finish_jobs(sim);
            assert_eq!(charged(sim, &["x/f", "y/h", "z/k"]), ["x/f"]);
            let before = sim.model.tree.total(find(sim, "")).alloc;
            delete(sim, &["x"]);
            assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
            assert!(sim.delete_next(dfdu_clean::DeleteResult::Deleted).unwrap());
            sim.applier_work().unwrap();
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
            // Only the directory `x` itself is gone: the data is still referenced.
            assert_eq!(sim.model.tree.total(find(sim, "")).alloc, before - 4096);
            rescan(sim, "y");
            finish_jobs(sim);
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
            rescan(sim, "z");
            finish_jobs(sim);
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
        },
    );
}

/// A Nix-like tree: a store of 40 files and 50 profiles linking each of them (2,000 links).
/// Rescanning the profiles swaps in a scope of 2,000 links. The merge runs in steps, with the
/// invariants holding after each, and every inode ends with one charge.
#[test]
fn s11_dense_links_swapped_in() {
    let spec = || {
        let mut b = SpecBuilder::new();
        let store = b.dir(dfdu_sim::ROOT, "store");
        let files = b.files(store, "s", 40);
        let profiles = b.dir(dfdu_sim::ROOT, "profiles");
        for p in 0..50 {
            let d = b.dir(profiles, &format!("p{p}"));
            for (i, &f) in files.iter().enumerate() {
                b.link(d, &format!("l{i}"), f);
            }
        }
        b.build()
    };
    let mut cfg = cfg();
    cfg.limits.block_entries = 64;
    quiet_injected_panics();
    for seed in 0..3 {
        let mut sim = Sim::with_spec(seed, &cfg, spec());
        sim.record = true;
        finish_jobs(&mut sim);
        rescan(&mut sim, "profiles");
        // Until the job finishes, then count the steps of the merge.
        while sim.model.status.running() > 0 {
            for w in 0..sim.workers.len() {
                sim.step_worker(w);
            }
            sim.apply_all().unwrap();
            sim.watchdog();
            if sim.applier.has_work() {
                break;
            }
        }
        let mut steps = 0;
        while sim.applier.has_work() {
            sim.applier.work_step(&mut sim.model);
            sim.applier.refresh_status(&mut sim.model);
            sim.applier.check(&sim.model).unwrap();
            steps += 1;
            if steps % 64 == 0 {
                for w in 0..sim.workers.len() {
                    sim.step_worker(w);
                }
                sim.apply_all().unwrap();
            }
        }
        finish_jobs(&mut sim);
        assert!(steps > 10, "the swap ran in {steps} steps");
        let t = &sim.model.tree;
        let root = find(&sim, "");
        let store_alloc: u64 = (0..40)
            .map(|i| (1000 + i as u64).div_ceil(4096) * 4096)
            .sum();
        let dirs = (1 + 1 + 1 + 50) * 4096;
        assert_eq!(t.total(root).alloc, store_alloc + dirs);
        if let Err(e) = sim.run_to_end() {
            panic!("{e}");
        }
    }
}

// ---- 14 ----------------------------------------------------------------------------------

/// A mark on a file that is replaced by a different file with the same name before a rescan
/// swap. After the swap the mark's reference no longer resolves, and found again by path the
/// new file's identity differs, so the mark is reported gone instead of moving to it. A mark
/// on a file that was not replaced follows it.
#[test]
fn s14_mark_on_replaced_file_is_gone_after_swap() {
    let spec = || {
        let mut b = SpecBuilder::new();
        let w = b.dir(dfdu_sim::ROOT, "w");
        b.file(w, "f", 100);
        let new = b.file(w, "f", 200);
        b.absent(new);
        b.file(w, "g", 300);
        b.build()
    };
    for_seeds(spec, cfg(), |sim| {
        finish_jobs(sim);
        let t = &sim.model.tree;
        let (f, g) = (
            dfdu_tui::Target::of(t, find(sim, "w/f")),
            dfdu_tui::Target::of(t, find(sim, "w/g")),
        );
        let w = sim.spec.nodes[0].children[0];
        let kids = sim.spec.nodes[w as usize].children.clone();
        sim.spec.delete_subtree(kids[0]);
        sim.spec.restore(kids[1]);
        rescan(sim, "w");
        finish_jobs(sim);
        let t = &sim.model.tree;
        assert_eq!(
            t.raw_size(find(sim, "w/f")).0,
            200,
            "the new f is in the tree"
        );
        assert_eq!(f.resolve(t), None, "the mark on the old f is gone");
        assert_eq!(
            g.resolve(t),
            Some(find(sim, "w/g")),
            "the mark on g follows it"
        );
    });
}

// ---- 15 ----------------------------------------------------------------------------------

/// Cancelling the initial scan while a rescan inside it runs: the rescan is independent; it
/// finishes and swaps in.
#[test]
fn s15_cancel_initial_scan_while_rescan_runs() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        rescan(sim, "a");
        let initial = sim.model.status.jobs[0].id;
        sim.command(dfdu_engine::Command::Cancel { job: initial })
            .unwrap();
        sim.applier_work().unwrap();
        assert_eq!(rescans_running(sim), 1);
        finish_jobs(sim);
        assert_eq!(state(sim, "a"), DirState::Complete);
        assert_eq!(state(sim, "a/b/c"), DirState::Complete);
    });
}

/// Cancelling a rescan whose target was superseded from the initial scan: the shadow is
/// dropped and the target stays visible, cut and incomplete; the initial scan goes on.
#[test]
fn s15_cancel_rescan_of_superseded_directory() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        rescan(sim, "a");
        assert_eq!(state(sim, "a"), DirState::Superseded);
        let job = sim.model.status.jobs[1].id;
        sim.command(dfdu_engine::Command::Cancel { job }).unwrap();
        sim.applier_work().unwrap();
        assert_eq!(rescans_running(sim), 0);
        let a = sim.model.tree.dir(find(sim, "a")).unwrap();
        assert!(a.cut && a.state == DirState::Superseded);
        assert_eq!(sim.model.tree.roots().count(), 1);
        assert_eq!(
            sim.model.status.jobs[0].phase,
            dfdu_engine::JobPhase::Running
        );
    });
}
