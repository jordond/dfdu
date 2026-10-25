//! The applier thread and the inbox it reads from.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use dut_scan::runtime::{Pool, RealClock};
use dut_scan::{Backend, Batch, Budget, Clock, Limits, ScanMsg, Shared, Sink};
use parking_lot::RwLock;

use crate::applier::{Applier, Command, Model};

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

#[derive(Clone, Debug)]
pub struct EngineOptions {
    pub workers: usize,
    pub budget_bytes: usize,
    pub limits: Limits,
    /// Abandon stalled directories automatically after this long (headless mode).
    pub auto_abandon: Option<Duration>,
    /// Check every invariant after every slice. Linear in tree size; for tests and debugging.
    pub check_invariants: bool,
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
        let (m, sh, st) = (model.clone(), shared.clone(), stop.clone());
        let applier = thread::Builder::new()
            .name("dut-applier".into())
            .spawn(move || applier_loop(m, sh, cmd_rx, inbox_rx, budget, wake_tx, st, opts))
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
        }
    }

    pub fn send(&self, cmd: Command) {
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
            if self.read(|m| m.status.commands_applied >= sent && !m.status.busy()) {
                return true;
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
    }
}

#[allow(clippy::too_many_arguments)]
fn applier_loop<B: Backend>(
    model: Arc<RwLock<Model>>,
    shared: Arc<Shared<B>>,
    cmd_rx: Receiver<Command>,
    inbox_rx: Receiver<Batch>,
    budget: Arc<Budget>,
    wake_tx: Sender<()>,
    stop: Arc<AtomicBool>,
    opts: EngineOptions,
) {
    let clock = RealClock::new();
    let mut applier = Applier::new(shared);
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut cmds: VecDeque<Command> = VecDeque::new();
    let auto_abandon = opts.auto_abandon.map(|d| d.as_nanos() as u64);
    while !stop.load(SeqCst) {
        if !applier.has_work() && cur.is_none() && cmds.is_empty() {
            select! {
                recv(cmd_rx) -> c => if let Ok(c) = c { cmds.push_back(c) },
                recv(inbox_rx) -> b => if let Ok(b) = b { cur = Some((b.msgs.into(), b.bytes)) },
                default(Duration::from_millis(50)) => {},
            }
        }
        let started = Instant::now();
        let mut g = model.write();
        let now = clock.now();
        while let Ok(c) = cmd_rx.try_recv() {
            cmds.push_back(c);
        }
        for c in cmds.drain(..) {
            applier.command(&mut g, c, now);
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
        drop(g);
        let _ = wake_tx.try_send(());
    }
}
