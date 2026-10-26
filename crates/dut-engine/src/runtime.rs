//! The applier thread and the inbox it reads from.

use dut_scan::panic_message;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use dut_clean::{DeleterMsg, Plan, spawn_deleter};
use dut_scan::runtime::{Pool, RealClock};
use dut_scan::{Backend, Batch, Budget, Clock, Limits, ScanMsg, Shared, Sink};
use parking_lot::RwLock;

use crate::applier::{Applier, Command, Model};
use crate::measure::{MeasureMsg, MeasureReq, spawn_measurers};

/// The applier's inbox: one channel, so control messages keep their order relative to data,
/// plus the byte budget that bounds data in transit (§4.3).
pub struct Inbox {
    tx: Sender<Batch>,
    budget: Arc<Budget>,
}

impl Inbox {
    pub fn new(budget_bytes: usize) -> (Inbox, Receiver<Batch>, Arc<Budget>) {
        let (tx, rx) = unbounded();
        let budget = Arc::new(Budget::new(budget_bytes));
        (
            Inbox {
                tx,
                budget: budget.clone(),
            },
            rx,
            budget,
        )
    }
}

impl Sink for Inbox {
    fn try_send(&self, mut batch: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(batch.bytes) {
            Some(n) => {
                batch.bytes = n;
                let _ = self.tx.send(batch);
                Ok(())
            }
            None => Err(batch),
        }
    }

    fn send(&self, mut batch: Batch) {
        batch.bytes = self.budget.acquire(batch.bytes);
        let _ = self.tx.send(batch);
    }

    fn send_control(&self, msg: ScanMsg) {
        let _ = self.tx.send(Batch {
            msgs: vec![msg],
            bytes: 0,
        });
    }
}

/// The state directory's `crash` folder, next to the deletion journal's `ops` folder.
pub fn default_crash_dir() -> Option<PathBuf> {
    dut_clean::default_journal_dir().and_then(|d| d.parent().map(|p| p.join("crash")))
}

fn crash_report(msg: &str, m: &Model) -> String {
    format!(
        "dut {} crash report\n\npanic: {msg}\n\nbacktrace:\n{}\n\nstats: {:?}\nanomalies: {:?}\njobs: {:?}\n",
        env!("CARGO_PKG_VERSION"),
        std::backtrace::Backtrace::force_capture(),
        m.status.stats,
        m.status.anomalies,
        m.status.jobs,
    )
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
    /// Threads measuring extents on filesystems that share them (`docs/btrfs.md` §3.4); 0
    /// turns measurement off and sizes are what `du` reports.
    pub measure_workers: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            workers: thread::available_parallelism()
                .map_or(4, |n| n.get())
                .clamp(2, 16),
            budget_bytes: 64 * 1024 * 1024,
            limits: Limits::default(),
            auto_abandon: None,
            check_invariants: std::env::var_os("DUT_CHECK").is_some(),
            journal_dir: dut_clean::default_journal_dir(),
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
}

/// Longest time the applier holds the write lock in one slice (§5.6).
const SLICE: Duration = Duration::from_millis(1);

impl<B: Backend> Engine<B> {
    pub fn start(backend: B, opts: EngineOptions) -> Self {
        let shared = Arc::new(Shared::new(backend, opts.limits.clone()));
        let (inbox, inbox_rx, budget) = Inbox::new(opts.budget_bytes);
        let inbox = Arc::new(inbox);
        let model = Arc::new(RwLock::new(Model::default()));
        let (cmd_tx, cmd_rx) = unbounded::<Command>();
        let (wake_tx, wake_rx) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));

        let pool = Pool::start(shared.clone(), inbox.clone(), opts.workers);
        let (del_tx, del_rx) = unbounded::<DeleterMsg>();
        let deleter = spawn_deleter(opts.journal_dir.clone(), del_tx);
        let (meas_tx, meas_rx) = unbounded::<Vec<MeasureMsg>>();
        let measurers =
            (opts.measure_workers > 0).then(|| spawn_measurers(opts.measure_workers, meas_tx));
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
            wake_tx,
        };
        let wake = chans.wake_tx.clone();
        let applier = thread::Builder::new()
            .name("dut-applier".into())
            .spawn(move || {
                let crash_dir = opts.crash_dir.clone();
                let model = m.clone();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    applier_loop(m, sh, chans, budget, st, opts)
                }));
                if let Err(p) = r {
                    // An applier panic is a bug. Record it where readers will see it, write a
                    // crash report, and stop changing the model (§8).
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

    /// Runs `f` under the read lock. Do no I/O inside `f` (§3 rules 3-4).
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
    /// stuck in a filesystem call are left behind (§8).
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
        wake_tx,
    } = ch;
    let clock = RealClock::new();
    let mut applier = Applier::new(shared);
    applier.read_only = opts.read_only;
    applier.measure = measure.is_some();
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
        {
            select! {
                recv(cmd_rx) -> c => if let Ok(c) = c { cmds.push_back(c) },
                recv(del_rx) -> d => if let Ok(d) = d { dels.push_back(d) },
                recv(meas_rx) -> r => if let Ok(r) = r { meas.extend(r) },
                recv(inbox_rx) -> b => if let Ok(b) = b { cur = Some((b.msgs.into(), b.bytes)) },
                default(Duration::from_millis(50)) => {},
            }
        }
        let started = Instant::now();
        let mut g = model.write();
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
        let changed = g.fingerprint() != before;
        drop(g);
        for p in outgoing {
            let _ = plans.send(p);
        }
        if let Some(tx) = &measure {
            for r in requests {
                let _ = tx.send(r);
            }
        }
        if changed {
            let _ = wake_tx.try_send(());
        }
    }
}
