//! The applier thread and the inbox it reads from.

use dfdu_scan::panic_message;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use dfdu_clean::{DeleterMsg, Plan, spawn_deleter};
use dfdu_scan::runtime::{Pool, RealClock};
use dfdu_scan::{Backend, Batch, Budget, Clock, Limits, ScanMsg, Shared, Sink};
use parking_lot::{RwLock, RwLockWriteGuard};

use crate::applier::{Applier, Command, Model, Timing};
use crate::classifier::{CheckMsg, CheckReq, spawn_classifiers};
use crate::measure::{MeasureMsg, MeasureReq, spawn_measurers};

/// The applier's inbox: one channel, so control messages keep their order relative to data,
/// plus the byte budget that bounds data in transit.
struct Inbox {
    tx: Sender<Batch>,
    budget: Arc<Budget>,
    /// Messages sent so far, counted before they enter the channel: a reader that sees the
    /// applier's count of messages received under the lock sees at least as many sent.
    sent: Arc<AtomicU64>,
}

impl Inbox {
    fn new(budget_bytes: usize) -> (Inbox, Receiver<Batch>, Arc<Budget>, Arc<AtomicU64>) {
        let (tx, rx) = unbounded();
        let budget = Arc::new(Budget::new(budget_bytes));
        let sent = Arc::new(AtomicU64::new(0));
        (
            Inbox {
                tx,
                budget: budget.clone(),
                sent: sent.clone(),
            },
            rx,
            budget,
            sent,
        )
    }

    fn push(&self, batch: Batch) {
        self.sent.fetch_add(batch.msgs.len() as u64, Relaxed);
        let _ = self.tx.send(batch);
    }
}

impl Sink for Inbox {
    fn try_send(&self, mut batch: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(batch.bytes) {
            Some(n) => {
                batch.bytes = n;
                self.push(batch);
                Ok(())
            }
            None => Err(batch),
        }
    }

    fn send(&self, mut batch: Batch) {
        batch.bytes = self.budget.acquire(batch.bytes);
        self.push(batch);
    }

    fn send_control(&self, msg: ScanMsg) {
        self.push(Batch {
            msgs: vec![msg],
            bytes: 0,
        });
    }
}

/// The state directory's `crash` folder, next to the deletion journal's `ops` folder.
pub fn default_crash_dir() -> Option<PathBuf> {
    dfdu_clean::default_journal_dir().and_then(|d| d.parent().map(|p| p.join("crash")))
}

fn crash_report(msg: &str, m: &Model) -> String {
    format!(
        "dfdu {} crash report\n\nthread: dfdu-applier\npanic: {msg}\n\nbacktrace:\n{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        std::backtrace::Backtrace::force_capture(),
        crash_details(m),
    )
}

/// The engine's part of a crash report: counters, anomalies, jobs and the last messages and
/// commands applied.
fn crash_details(m: &Model) -> String {
    format!(
        "stats: {:?}\nanomalies: {:?}\njobs: {:?}\n\n{}",
        m.status.stats,
        m.status.anomalies,
        m.status.jobs,
        m.recent.report(),
    )
}

/// The model of the engine started last, for a panic hook on another thread.
static CRASH_MODEL: parking_lot::Mutex<Option<Weak<RwLock<Model>>>> = parking_lot::Mutex::new(None);

/// `crash_details` of the engine started last, for a panic on a thread other than the
/// applier (the UI); `None` if no engine is running. Waits at most 500 ms for the read lock: a
/// thread that panics while holding it would wait behind the applier's pending write forever.
pub fn crash_state() -> Option<String> {
    let model = CRASH_MODEL
        .try_lock_for(Duration::from_millis(100))?
        .as_ref()?
        .upgrade()?;
    Some(match model.try_read_for(Duration::from_millis(500)) {
        Some(g) => crash_details(&g),
        None => "engine state: not read, the model stayed locked for 500 ms\n".to_string(),
    })
}

/// Whether a panic on the thread with this name is caught and recorded by the engine: a scan
/// worker's (recorded as a `Panicked` directory) or the applier's (recorded as the engine's fatal
/// error, with its own crash report). A panic hook that writes a crash report or restores the
/// terminal must leave these alone.
pub fn panic_is_caught_by_engine(thread_name: Option<&str>) -> bool {
    matches!(thread_name, Some("dfdu-worker" | "dfdu-applier"))
}

/// Writes a crash report into `dir`; returns its path.
pub fn write_crash_report(dir: &std::path::Path, report: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let path = dir.join(format!("crash-{}-{}.txt", t.as_secs(), std::process::id()));
    std::fs::write(&path, report).ok()?;
    Some(path)
}

#[derive(Clone, Debug)]
pub struct EngineOptions {
    /// Worker threads, chosen for the scan root's storage ([`workers_for`]). A scan that enters
    /// a mount with its own limit (a spinning disk, a network mount) adds that many workers,
    /// at most [`dfdu_scan::MAX_MOUNT_WORKERS`].
    pub workers: usize,
    pub budget_bytes: usize,
    pub limits: Limits,
    /// Abandon stalled directories automatically after this long (headless mode).
    pub auto_abandon: Option<Duration>,
    /// Check every invariant after every slice. Linear in tree size; for tests and debugging.
    pub check_invariants: bool,
    /// Where the deletion journal goes. Without one, deletion is refused.
    pub journal_dir: Option<PathBuf>,
    /// Refuse all deletion.
    pub read_only: bool,
    /// Where crash reports go.
    pub crash_dir: Option<PathBuf>,
    /// Threads measuring extents on filesystems that share them; 0 turns measurement off and
    /// sizes are what `du` reports.
    pub measure_workers: usize,
    /// Classifier threads checking matches against their guards (git tracking, running
    /// processes); 0 turns the checks off.
    pub classifiers: usize,
}

/// Worker threads for data on `storage`: two on a spinning disk, where more threads only add
/// seeks; otherwise one per core, 2 to 16.
pub fn workers_for(storage: dfdu_sys::Storage) -> usize {
    match storage {
        dfdu_sys::Storage::Rotational => 2,
        _ => thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(2, 16),
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            workers: workers_for(dfdu_sys::Storage::Unknown),
            budget_bytes: 64 * 1024 * 1024,
            limits: Limits::default(),
            auto_abandon: None,
            check_invariants: std::env::var_os("DFDU_CHECK").is_some(),
            journal_dir: dfdu_clean::default_journal_dir(),
            read_only: false,
            crash_dir: default_crash_dir(),
            // The kernel's extent lookups are the cost, and they scale with threads.
            measure_workers: if cfg!(target_os = "linux") {
                thread::available_parallelism()
                    .map_or(2, |n| n.get())
                    .clamp(2, 8)
            } else {
                0
            },
            classifiers: 2,
        }
    }
}

pub struct Engine<B: Backend> {
    model: Arc<RwLock<Model>>,
    shared: Arc<Shared<B>>,
    cmd_tx: Sender<Command>,
    commands_sent: AtomicU64,
    wake_rx: Receiver<()>,
    stop: Arc<AtomicBool>,
    applier: Option<JoinHandle<()>>,
    pool: Option<Pool>,
    delete_abort: Arc<AtomicBool>,
    deleter: Option<JoinHandle<()>>,
    budget: Arc<Budget>,
    sent: Arc<AtomicU64>,
}

/// Longest time the applier holds the write lock in one slice.
const SLICE: Duration = Duration::from_millis(1);

/// A slice still holding the lock this long after it started has probably been preempted:
/// workers then yield their CPUs to it (`dfdu_scan::late`). The last step of a slice can take a
/// few hundred µs past [`SLICE`].
const LATE: Duration = Duration::from_micros(1200);

/// Waiting this long for the lock means the applier was probably woken but not given a CPU:
/// readers hold the lock for a few ms at most (a UI frame; 5 ms in the soak test's reader).
const WAIT_LATE: Duration = Duration::from_millis(6);

impl<B: Backend> Engine<B> {
    pub fn start(backend: B, opts: EngineOptions) -> Self {
        let mut limits = opts.limits.clone();
        if limits.chunk_credit == 0 {
            limits.chunk_credit = opts.workers.max(2);
        }
        if limits.pool_workers == 0 {
            limits.pool_workers = opts.workers.max(1);
        }
        let shared = Arc::new(Shared::new(backend, limits));
        let (inbox, inbox_rx, budget, sent) = Inbox::new(opts.budget_bytes);
        let transit = budget.clone();
        let inbox = Arc::new(inbox);
        let model = Arc::new(RwLock::new(Model::default()));
        *CRASH_MODEL.lock() = Some(Arc::downgrade(&model));
        let (cmd_tx, cmd_rx) = unbounded::<Command>();
        let (wake_tx, wake_rx) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));

        let pool = Pool::start(shared.clone(), inbox.clone(), opts.workers);
        let (del_tx, del_rx) = unbounded::<DeleterMsg>();
        let deleter = spawn_deleter(opts.journal_dir.clone(), del_tx);
        let (meas_tx, meas_rx) = unbounded::<Vec<MeasureMsg>>();
        let measurers =
            (opts.measure_workers > 0).then(|| spawn_measurers(opts.measure_workers, meas_tx));
        // Without measurers the sender is dropped, and a receiver with no senders is always
        // ready: the applier's `select!` would return at once and it would take the write lock
        // in a loop (measurement is off on macOS and Windows).
        let meas_rx = if measurers.is_some() {
            meas_rx
        } else {
            crossbeam_channel::never()
        };
        let (chk_tx, chk_rx) = unbounded::<Vec<CheckMsg>>();
        let classifiers =
            (opts.classifiers > 0).then(|| spawn_classifiers(opts.classifiers, chk_tx));
        let chk_rx = if classifiers.is_some() {
            chk_rx
        } else {
            crossbeam_channel::never()
        };
        let delete_abort = deleter.abort.clone();
        let deleter_thread = deleter.thread;
        let (m, sh, st) = (model.clone(), shared.clone(), stop.clone());
        let chans = Channels {
            cmd_rx,
            inbox_rx,
            del_rx,
            plans: deleter.plans,
            meas_rx,
            measure: measurers,
            chk_rx,
            classify: classifiers,
            wake_tx,
        };
        let wake = chans.wake_tx.clone();
        let applier = thread::Builder::new()
            .name("dfdu-applier".into())
            .spawn(move || {
                let crash_dir = opts.crash_dir.clone();
                let model = m.clone();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    applier_loop(m, sh, chans, budget, st, opts)
                }));
                if let Err(p) = r {
                    // An applier panic is a bug. Record it where readers will see it, write a
                    // crash report, and stop changing the model.
                    let msg = panic_message(&*p);
                    let mut g = model.write();
                    let report = crash_report(&msg, &g);
                    let file = crash_dir
                        .as_deref()
                        .and_then(|d| write_crash_report(d, &report));
                    g.status.fatal = Some(match file {
                        Some(f) => format!("internal error: {msg} (report: {})", f.display()),
                        None => format!("internal error: {msg}"),
                    });
                    drop(g);
                    let _ = wake.try_send(());
                }
            })
            .expect("spawn applier");
        Engine {
            model,
            shared,
            cmd_tx,
            commands_sent: AtomicU64::new(0),
            wake_rx,
            stop,
            applier: Some(applier),
            pool: Some(pool),
            delete_abort,
            deleter: Some(deleter_thread),
            budget: transit,
            sent,
        }
    }

    pub fn send(&self, cmd: Command) {
        if matches!(cmd, Command::AbortDelete) {
            self.delete_abort.store(true, SeqCst);
        }
        // Scan roots are made absolute and canonical here, on the caller's thread, so the
        // applier never does I/O and deletion can require absolute paths inside the root.
        let cmd = match cmd {
            Command::Scan { path, opts } => {
                // dunce: on Windows, the plain C:\ form where it is equivalent, instead of the
                // \\?\ verbatim form std returns.
                let path = dunce::canonicalize(&path)
                    .or_else(|_| std::path::absolute(&path))
                    .unwrap_or(path);
                Command::Scan { path, opts }
            }
            other => other,
        };
        self.commands_sent.fetch_add(1, SeqCst);
        let _ = self.cmd_tx.send(cmd);
    }

    /// Runs `f` under the read lock. Do no I/O inside `f`: the applier waits for the lock.
    pub fn read<R>(&self, f: impl FnOnce(&Model) -> R) -> R {
        f(&self.model.read())
    }

    /// Receives a message whenever the model changed.
    pub fn wake(&self) -> &Receiver<()> {
        &self.wake_rx
    }

    pub fn scanner(&self) -> &Shared<B> {
        &self.shared
    }

    /// The byte budget on scanner data in transit to the applier.
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Scanner messages sent to the applier so far. Those not yet in
    /// [`Stats::messages`](crate::Stats::messages) are in transit; read under the lock, the
    /// count is never below it.
    pub fn sent_messages(&self) -> u64 {
        self.sent.load(Relaxed)
    }

    /// Blocks until every command sent so far has been applied, no job is running and no
    /// incremental work is pending, or `timeout` passes.
    pub fn wait_idle(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let sent = self.commands_sent.load(SeqCst);
            let (idle, fatal) = self.read(|m| {
                (
                    m.status.commands_applied >= sent && !m.status.busy(),
                    m.status.fatal.is_some(),
                )
            });
            if idle {
                return true;
            }
            if fatal {
                return false;
            }
            let wait = deadline.map_or(Duration::from_millis(100), |d| {
                d.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100))
            });
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            let _ = self.wake_rx.recv_timeout(wait);
        }
    }

    /// Cancels everything and stops the threads, waiting at most `wait` for workers. Threads
    /// stuck in a filesystem call are left behind.
    pub fn shutdown(mut self, wait: Duration) {
        self.send(Command::CancelAll);
        self.stop.store(true, SeqCst);
        let _ = self.cmd_tx.send(Command::CancelAll);
        if let Some(a) = self.applier.take() {
            let _ = a.join();
        }
        if let Some(p) = self.pool.take() {
            p.stop(wait);
        }
        // The applier held the deleter's plan channel; with it gone the deleter finishes its
        // current item, syncs and closes the journal, and exits. Wait for that, within `wait`.
        self.delete_abort.store(true, SeqCst);
        if let Some(d) = self.deleter.take() {
            let deadline = Instant::now() + wait;
            while !d.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if d.is_finished() {
                let _ = d.join();
            }
        }
    }
}

struct Channels {
    cmd_rx: Receiver<Command>,
    inbox_rx: Receiver<Batch>,
    del_rx: Receiver<DeleterMsg>,
    plans: Sender<Plan>,
    meas_rx: Receiver<Vec<MeasureMsg>>,
    /// `None` when measurement is off. Dropping it ends the measurer threads.
    measure: Option<Sender<MeasureReq>>,
    chk_rx: Receiver<Vec<CheckMsg>>,
    /// `None` when the classifier pool is off.
    classify: Option<Sender<Vec<CheckReq>>>,
    wake_tx: Sender<()>,
}

fn applier_loop<B: Backend>(
    model: Arc<RwLock<Model>>,
    shared: Arc<Shared<B>>,
    ch: Channels,
    budget: Arc<Budget>,
    stop: Arc<AtomicBool>,
    opts: EngineOptions,
) {
    let Channels {
        cmd_rx,
        inbox_rx,
        del_rx,
        plans,
        meas_rx,
        measure,
        chk_rx,
        classify,
        wake_tx,
    } = ch;
    let clock = RealClock::new();
    let mut applier = Applier::new(shared);
    applier.read_only = opts.read_only;
    applier.measure = measure.is_some();
    applier.checks = classify.is_some();
    let mut chks: VecDeque<CheckMsg> = VecDeque::new();
    let mut dels: VecDeque<DeleterMsg> = VecDeque::new();
    let mut meas: VecDeque<MeasureMsg> = VecDeque::new();
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut cmds: VecDeque<Command> = VecDeque::new();
    let auto_abandon = opts.auto_abandon.map(|d| d.as_nanos() as u64);
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
            applier.command(&mut g, c, now);
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
        loop {
            if applier.has_work() {
                applier.work_step(&mut g);
            } else {
                if cur.is_none() {
                    match inbox_rx.try_recv() {
                        Ok(b) => cur = Some((b.msgs.into(), b.bytes)),
                        Err(_) => break,
                    }
                }
                let (msgs, bytes) = cur.as_mut().expect("batch");
                match msgs.pop_front() {
                    Some(msg) => applier.apply(&mut g, msg, now),
                    None => {
                        budget.release(*bytes);
                        cur = None;
                    }
                }
            }
            if started.elapsed() >= SLICE {
                break;
            }
        }
        applier.tick(&mut g, now, auto_abandon);
        applier.refresh_status(&mut g);
        if opts.check_invariants
            && let Err(e) = applier.check(&g)
        {
            g.status.incomplete = true;
            g.status.notices.push(format!("invariant violated: {e}"));
        }
        let outgoing = applier.take_plans();
        let requests = applier.take_measure_requests();
        let checks = applier.take_check_requests();
        let changed = g.fingerprint() != before;
        let held = started.elapsed();
        let t = &mut g.status.timing;
        t.slices += 1;
        t.locked_ns += held.as_nanos() as u64;
        t.hold[Timing::bucket(held)] += 1;
        t.wait[Timing::bucket(started - asked)] += 1;
        // A plain unlock lets this thread take the lock again for the next slice before a
        // waiting reader wakes up, so with work queued back to back (the end of a scan) a reader
        // can wait tens of ms. A fair unlock hands the lock to the waiting reader.
        dfdu_scan::late::clear();
        RwLockWriteGuard::unlock_fair(g);
        for p in outgoing {
            let _ = plans.send(p);
        }
        if let Some(tx) = &measure {
            for r in requests {
                let _ = tx.send(r);
            }
        }
        if let Some(tx) = &classify
            && !checks.is_empty()
        {
            let _ = tx.send(checks);
        }
        if changed {
            let _ = wake_tx.try_send(());
        }
    }
}
