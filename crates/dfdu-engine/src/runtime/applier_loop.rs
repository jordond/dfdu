//! The applier thread's loop: one slice under the model's write lock at a time, applying
//! commands, results and scanner messages, then sending what the slice asked for.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use dfdu_clean::{DeleterMsg, DeleterReq};
use dfdu_scan::runtime::RealClock;
use dfdu_scan::{Backend, Batch, Budget, Clock, ScanMsg, Shared};
use parking_lot::{RwLock, RwLockWriteGuard};

use super::EngineOptions;
use crate::applier::{Applier, Command, Model};
use crate::classifier::{CheckMsg, CheckReq};
use crate::measure::{MeasureMsg, MeasureReq};
use crate::nanos::nanos;

/// Longest time the applier holds the write lock in one slice.
const SLICE: Duration = Duration::from_millis(1);

/// A slice still holding the lock this long after it started has probably been preempted:
/// workers then yield their CPUs to it (`dfdu_scan::late`). The last step of a slice can take a
/// few hundred µs past [`SLICE`].
const LATE: Duration = Duration::from_micros(1200);

/// Waiting this long for the lock means the applier was probably woken but not given a CPU:
/// readers hold the lock for a few ms at most (a UI frame; 5 ms in the soak test's reader).
const WAIT_LATE: Duration = Duration::from_millis(6);

/// The applier thread's ends of the engine's channels.
pub(super) struct Channels {
    pub(super) cmd_rx: Receiver<Command>,
    pub(super) inbox_rx: Receiver<Batch>,
    pub(super) del_rx: Receiver<DeleterMsg>,
    pub(super) plans: Sender<DeleterReq>,
    pub(super) meas_rx: Receiver<Vec<MeasureMsg>>,
    /// `None` when measurement is off. Dropping it ends the measurer threads.
    pub(super) measure: Option<Sender<MeasureReq>>,
    /// Files allocating less are not measured.
    pub(super) measure_min: u64,
    /// Whoever deletes runs as root (the agent, for a remote engine).
    pub(super) privileged: bool,
    pub(super) chk_rx: Receiver<Vec<CheckMsg>>,
    /// `None` when the classifier pool is off.
    pub(super) classify: Option<Sender<Vec<CheckReq>>>,
    pub(super) wake_tx: Sender<()>,
}

/// What a slice asks of the deleter, the measurers and the classifiers, sent once the write
/// lock is released.
struct Outgoing {
    deleter: Vec<DeleterReq>,
    measure: Vec<MeasureReq>,
    checks: Vec<CheckReq>,
}

impl Outgoing {
    fn take<B: Backend>(applier: &mut Applier<B>) -> Self {
        let mut deleter: Vec<DeleterReq> = applier
            .take_plans()
            .into_iter()
            .map(DeleterReq::Plan)
            .collect();
        deleter.extend(applier.take_deleter_controls());
        Outgoing {
            deleter,
            measure: applier.take_measure_requests(),
            checks: applier.take_check_requests(),
        }
    }

    /// Sends the requests; those for a pool that is off are dropped.
    fn send(
        self,
        deleter: &Sender<DeleterReq>,
        measure: Option<&Sender<MeasureReq>>,
        classify: Option<&Sender<Vec<CheckReq>>>,
    ) {
        for p in self.deleter {
            let _ = deleter.send(p);
        }
        if let Some(tx) = measure {
            for r in self.measure {
                let _ = tx.send(r);
            }
        }
        if let Some(tx) = classify
            && !self.checks.is_empty()
        {
            let _ = tx.send(self.checks);
        }
    }
}

/// Applies scanner messages from the inbox (`cur` the batch being applied) and incremental
/// work, until neither is left or the slice that began at `started` is used up.
fn apply_inbox<B: Backend>(
    applier: &mut Applier<B>,
    g: &mut Model,
    cur: &mut Option<(VecDeque<ScanMsg>, usize)>,
    inbox_rx: &Receiver<Batch>,
    budget: &Budget,
    now: u64,
    started: Instant,
) {
    loop {
        if applier.has_work() {
            applier.work_step(g);
        } else {
            let (msgs, bytes) = match cur {
                Some(batch) => batch,
                None => match inbox_rx.try_recv() {
                    Ok(b) => cur.insert((b.msgs.into(), b.bytes)),
                    Err(_) => break,
                },
            };
            if let Some(msg) = msgs.pop_front() {
                applier.apply(g, msg, now);
            } else {
                budget.release(*bytes);
                *cur = None;
            }
        }
        if started.elapsed() >= SLICE {
            break;
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one write-lock slice of the applier per iteration, its steps in order"
)]
pub(super) fn applier_loop<B: Backend>(
    model: &RwLock<Model>,
    shared: Arc<Shared<B>>,
    remote: Option<Arc<crate::remote::Link>>,
    ch: Channels,
    budget: &Budget,
    stop: &AtomicBool,
    opts: &EngineOptions,
) {
    let Channels {
        cmd_rx,
        inbox_rx,
        del_rx,
        plans,
        meas_rx,
        measure,
        measure_min,
        privileged,
        chk_rx,
        classify,
        wake_tx,
    } = ch;
    let clock = RealClock::new();
    let mut applier = Applier::new(shared);
    applier.remote = remote;
    applier.read_only = opts.read_only || opts.imported;
    applier.imported = opts.imported;
    applier.measure = measure.is_some();
    applier.measure_min = measure_min;
    applier.privileged = privileged;
    applier.checks = classify.is_some();
    let mut chks: VecDeque<CheckMsg> = VecDeque::new();
    let mut dels: VecDeque<DeleterMsg> = VecDeque::new();
    let mut meas: VecDeque<MeasureMsg> = VecDeque::new();
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut cmds: VecDeque<Command> = VecDeque::new();
    let auto_abandon = opts.auto_abandon.map(nanos);
    while !stop.load(SeqCst) {
        if !applier.has_work()
            && cur.is_none()
            && cmds.is_empty()
            && dels.is_empty()
            && meas.is_empty()
            && chks.is_empty()
        {
            select! {
                recv(cmd_rx) -> c => if let Ok(c) = c { cmds.push_back(c) },
                recv(del_rx) -> d => if let Ok(d) = d { dels.push_back(d) },
                recv(meas_rx) -> r => if let Ok(r) = r { meas.extend(r) },
                recv(chk_rx) -> r => if let Ok(r) = r { chks.extend(r) },
                recv(inbox_rx) -> b => if let Ok(b) = b { cur = Some((b.msgs.into(), b.bytes)) },
                default(Duration::from_millis(50)) => {},
            }
        }
        let asked = Instant::now();
        dfdu_scan::late::expect_lock_within(WAIT_LATE);
        let mut g = model.write();
        let started = Instant::now();
        dfdu_scan::late::expect_lock_within(LATE);
        let before = g.fingerprint();
        let now = clock.now();
        while let Ok(c) = cmd_rx.try_recv() {
            cmds.push_back(c);
        }
        for c in cmds.drain(..) {
            applier.apply_command(&mut g, c);
        }
        dels.extend(del_rx.try_iter());
        for d in dels.drain(..) {
            applier.apply_delete(&mut g, d);
        }
        chks.extend(chk_rx.try_iter().flatten());
        for c in chks.drain(..) {
            applier.apply_check(&mut g, c);
        }
        meas.extend(meas_rx.try_iter().flatten());
        while let Some(r) = meas.pop_front() {
            applier.apply_measure(&mut g, r, now);
            if started.elapsed() >= SLICE {
                break;
            }
        }
        apply_inbox(
            &mut applier,
            &mut g,
            &mut cur,
            &inbox_rx,
            budget,
            now,
            started,
        );
        if cur.is_none() && inbox_rx.is_empty() {
            applier.check_link(&mut g);
        }
        applier.tick(&mut g, now, auto_abandon);
        applier.refresh_status(&mut g);
        if opts.check_invariants
            && let Err(e) = applier.check(&g)
        {
            g.status.incomplete = true;
            g.status.notices.push(format!("invariant violated: {e}"));
        }
        let outgoing = Outgoing::take(&mut applier);
        let changed = g.fingerprint() != before;
        g.status.timing.record(started.elapsed(), started - asked);
        // A plain unlock lets this thread take the lock again for the next slice before a
        // waiting reader wakes up, so with work queued back to back (the end of a scan) a reader
        // can wait tens of ms. A fair unlock hands the lock to the waiting reader.
        dfdu_scan::late::clear();
        RwLockWriteGuard::unlock_fair(g);
        outgoing.send(&plans, measure.as_ref(), classify.as_ref());
        if changed {
            let _ = wake_tx.try_send(());
        }
    }
}
