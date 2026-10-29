//! The engine: its threads, the inbox the applier reads from, and the API over them.

mod applier_loop;

use dfdu_scan::panic_message;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use dfdu_clean::{DeleterMsg, spawn_deleter};
use dfdu_scan::journal::{JournalPosition, changed_since};
use dfdu_scan::runtime::Pool;
use dfdu_scan::{
    Backend, Batch, Budget, JobOptions, Limits, PortableBackend, ScanMsg, Shared, Sink,
};
use dfdu_tree::Tree;
use parking_lot::RwLock;

use crate::applier::{Command, Model};
use crate::classifier::{CheckMsg, spawn_classifiers};
use crate::crash::{CRASH_MODEL, default_crash_dir, record_applier_panic};
use crate::measure::{MeasureMsg, spawn_measurers};
use crate::threads::spawn;
use applier_loop::{Channels, applier_loop};

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

/// How an engine runs. [`Default`] reads nothing from the environment: no journal (deletion is
/// refused), no crash reports and no invariant checks. The commands start from
/// [`EngineOptions::from_env`].
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
    /// How long a staged item can be put back before it is deleted.
    pub stage_window: Duration,
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
/// seeks; otherwise one per core, 2 to 16. On a Mac with performance and efficiency cores,
/// one per performance core: on an M4 Max (12 + 4) a warm scan of 1M entries takes 2.0 s with
/// 10 or 12 workers, 2.5 s with 14 and 3.0 s with 16.
#[must_use]
pub fn workers_for(storage: dfdu_sys::Storage) -> usize {
    let all = || thread::available_parallelism().map_or(4, std::num::NonZero::get);
    #[cfg(target_os = "macos")]
    let cores = dfdu_sys::performance_cores().unwrap_or_else(all);
    #[cfg(not(target_os = "macos"))]
    let cores = all();
    match storage {
        dfdu_sys::Storage::Rotational => 2,
        _ => cores.clamp(2, 16),
    }
}

/// Measurer threads by default: none where nothing is measured.
pub(crate) fn default_measurers() -> usize {
    // The kernel's extent lookups are the cost, and they scale with threads.
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        thread::available_parallelism()
            .map_or(2, std::num::NonZero::get)
            .clamp(2, 8)
    } else {
        0
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            workers: workers_for(dfdu_sys::Storage::Unknown),
            budget_bytes: 64 * 1024 * 1024,
            limits: Limits::default(),
            auto_abandon: None,
            check_invariants: false,
            journal_dir: None,
            read_only: false,
            stage_window: dfdu_clean::STAGE_UNDO_WINDOW,
            crash_dir: None,
            measure_workers: default_measurers(),
            classifiers: 2,
        }
    }
}

impl EngineOptions {
    /// The defaults, with the deletion journal and crash reports in the user's state directory
    /// and invariant checks on when `DFDU_CHECK` is set.
    #[must_use]
    pub fn from_env() -> Self {
        EngineOptions {
            check_invariants: std::env::var_os("DFDU_CHECK").is_some(),
            journal_dir: dfdu_clean::default_journal_dir(),
            crash_dir: default_crash_dir(),
            ..EngineOptions::default()
        }
    }
}

/// How [`Engine::wait_idle`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitIdle {
    /// Every command sent was applied and nothing is left to do.
    Idle,
    /// The timeout passed first.
    TimedOut,
    /// The engine stopped on this fatal error.
    Fatal(String),
}

impl WaitIdle {
    #[must_use]
    pub fn is_idle(&self) -> bool {
        *self == WaitIdle::Idle
    }
}

pub struct Engine<B: Backend> {
    model: Arc<RwLock<Model>>,
    shared: Arc<Shared<B>>,
    cmd_tx: Sender<Command>,
    commands_sent: AtomicU64,
    changes_rx: Receiver<()>,
    stop: Arc<AtomicBool>,
    applier: Option<JoinHandle<()>>,
    pool: Option<Pool>,
    link: Option<Arc<crate::remote::Link>>,
    delete_abort: Arc<AtomicBool>,
    deleter: Option<JoinHandle<()>>,
    budget: Arc<Budget>,
    sent: Arc<AtomicU64>,
}

impl Engine<PortableBackend> {
    /// An engine over a tree read from a file, whose paths may be another machine's or no
    /// longer exist: nothing is scanned, rescanned, deleted or run on them.
    #[must_use]
    pub fn start_imported(tree: Tree, opts: EngineOptions) -> Self {
        Self::start_inner(
            PortableBackend::default(),
            opts,
            Source::Imported(Box::new(tree)),
        )
    }
}

impl<B: Backend> Engine<B> {
    pub fn start(backend: B, opts: EngineOptions) -> Self {
        Self::start_inner(backend, opts, Source::Local)
    }

    /// An engine whose scans run on another host, by the `dfdu agent` at the other end of `io`;
    /// `backend` is only consulted for local reads. Fails when the
    /// agent does not answer the greeting or speaks no common protocol version. With `rules`,
    /// the agent classifies with them; the answer says what the scan root classifies against,
    /// to put in [`Command::Scan`]'s options.
    ///
    /// # Errors
    ///
    /// If the agent does not greet back, speaks no common protocol version, or the link ends
    /// or carries something that is not a valid message before the greeting is over.
    pub fn start_remote(
        backend: B,
        opts: EngineOptions,
        io: crate::RemoteIo,
        rules: Option<crate::RemoteRules>,
    ) -> std::io::Result<(Self, crate::Connected)> {
        let (link, reader, connected) = crate::remote::connect(io, rules)?;
        // The tree's paths are the other host's: nothing here may act on them. Deletion, guard
        // checks and extent measurement run on the agent (`measure_workers` 0 still turns
        // measurement off); native commands would run on this host.
        let remote = Remote {
            link,
            reader,
            measure_min: connected.measure_min,
            privileged: connected.privileged,
        };
        Ok((
            Self::start_inner(backend, opts, Source::Remote(remote)),
            connected,
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the wiring of the engine's threads and channels, read top to bottom"
    )]
    fn start_inner(backend: B, opts: EngineOptions, source: Source) -> Self {
        let shared = Arc::new(Shared::new(backend, scan_limits(&opts)));
        let (inbox, inbox_rx, budget, sent) = Inbox::new(opts.budget_bytes);
        let transit = budget.clone();
        let inbox = Arc::new(inbox);
        let mut model = Model::default();
        let (remote, imported) = match source {
            Source::Local => (None, false),
            Source::Remote(r) => (Some(r), false),
            Source::Imported(tree) => {
                model.tree = *tree;
                model.tree.bump_version();
                (None, true)
            }
        };
        let model = Arc::new(RwLock::new(model));
        *CRASH_MODEL.lock() = Some(Arc::downgrade(&model));
        let (cmd_tx, cmd_rx) = unbounded::<Command>();
        let (wake_tx, changes_rx) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));

        let (link, reader, remote_min, privileged) = match remote {
            Some(r) => (
                Some(r.link),
                Some(r.reader),
                Some(r.measure_min),
                r.privileged,
            ),
            None => (None, None, None, dfdu_sys::is_privileged()),
        };
        let pool = link
            .is_none()
            .then(|| Pool::start(&shared, &inbox, opts.workers));
        let (del_tx, del_rx) = unbounded::<DeleterMsg>();
        // A remote engine's plans run on the agent; their results come in through the link.
        let deleter = match &link {
            Some(l) => crate::remote::spawn_deleter(l.clone(), del_tx.clone()),
            None => spawn_deleter(opts.journal_dir.clone(), opts.stage_window, del_tx.clone()),
        };
        let (meas_tx, meas_rx) = unbounded::<Vec<MeasureMsg>>();
        // A remote engine's measurements run on the agent, when it can measure; their answers
        // come in through the link.
        let measurers = match (&link, remote_min) {
            (Some(l), Some(min)) => (opts.measure_workers > 0 && min > 0)
                .then(|| crate::remote::spawn_measurer(l.clone(), meas_tx.clone())),
            _ => {
                (opts.measure_workers > 0).then(|| spawn_measurers(opts.measure_workers, &meas_tx))
            }
        };
        let measure_min = remote_min.unwrap_or(crate::applier::MEASURE_MIN);
        // Without measurers the sender is dropped (measurement is off on macOS and Windows).
        let meas_rx = or_never(meas_rx, measurers.is_some());
        let (chk_tx, chk_rx) = unbounded::<Vec<CheckMsg>>();
        // A remote engine's guard checks run on the agent; their answers come in through the
        // link.
        let classifiers = match &link {
            Some(l) => Some(crate::remote::spawn_checker(l.clone())),
            None => (opts.classifiers > 0).then(|| spawn_classifiers(opts.classifiers, &chk_tx)),
        };
        let chk_rx = or_never(chk_rx, classifiers.is_some());
        let delete_abort = deleter.abort.clone();
        let deleter_thread = deleter.thread;
        if let (Some(l), Some(r)) = (&link, reader) {
            let wake = wake_tx.clone();
            let answers = crate::remote::Answers {
                deleted: del_tx.clone(),
                checked: chk_tx,
                measured: meas_tx,
            };
            crate::remote::spawn_reader(
                l.clone(),
                r,
                inbox.clone(),
                answers,
                stop.clone(),
                move || {
                    let _ = wake.try_send(());
                },
            );
        }
        let (m, sh, st, ln) = (model.clone(), shared.clone(), stop.clone(), link.clone());
        let chans = Channels {
            cmd_rx,
            inbox_rx,
            del_rx,
            plans: deleter.reqs,
            meas_rx,
            measure: measurers,
            measure_min,
            privileged,
            imported,
            chk_rx,
            classify: classifiers,
            wake_tx,
        };
        let wake = chans.wake_tx.clone();
        let applier = spawn("dfdu-applier", move || {
            let crash_dir = opts.crash_dir.clone();
            let model = m.clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                applier_loop(&m, sh, ln, chans, &budget, &st, &opts);
            }));
            if let Err(p) = r {
                // An applier panic is a bug. Record it where readers will see it, write a
                // crash report, and stop changing the model.
                record_applier_panic(&model, crash_dir.as_deref(), &panic_message(&*p));
                let _ = wake.try_send(());
            }
        });
        Engine {
            model,
            shared,
            cmd_tx,
            commands_sent: AtomicU64::new(0),
            changes_rx,
            stop,
            applier: Some(applier),
            pool,
            link,
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

    /// Replaces the tree with one read from a file, before anything is scanned, to bring it up
    /// to date with [`Command::Resume`]. A tree that is only browsed is started with
    /// [`Engine::start_imported`] instead.
    pub fn load(&self, tree: Tree) {
        let mut m = self.model.write();
        m.tree = tree;
        m.tree.bump_version();
    }

    /// Loads `tree`, read from a file ([`Engine::load`]), and brings its root up to date with
    /// `opts` ([`Command::Resume`]): only the directories changed since `journal` when the
    /// volume's change journal can tell, otherwise all of it. A tree without a root is only
    /// loaded.
    pub fn resume(&self, tree: Tree, journal: Option<JournalPosition>, opts: JobOptions) {
        let Some(root) = tree.roots().next() else {
            self.load(tree);
            return;
        };
        let node = tree.node_ref(root);
        let changed = journal.and_then(|j| changed_since(&tree.path_buf(root), j));
        self.load(tree);
        self.send(Command::Resume {
            node,
            opts,
            changed,
        });
    }

    /// Receives a message whenever the model changed.
    pub fn changes(&self) -> &Receiver<()> {
        &self.changes_rx
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
    /// incremental work is pending, until `timeout` passes, or until the engine stops on a
    /// fatal error (an engine that stopped is never idle).
    pub fn wait_idle(&self, timeout: Option<Duration>) -> WaitIdle {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let sent = self.commands_sent.load(SeqCst);
            let (idle, fatal) = self.read(|m| {
                (
                    m.status.commands_applied >= sent && !m.status.busy(),
                    m.status.fatal.clone(),
                )
            });
            if let Some(msg) = fatal {
                return WaitIdle::Fatal(msg);
            }
            if idle {
                return WaitIdle::Idle;
            }
            let wait = deadline.map_or(Duration::from_millis(100), |d| {
                d.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100))
            });
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return WaitIdle::TimedOut;
            }
            let _ = self.changes_rx.recv_timeout(wait);
        }
    }

    /// Cancels everything and stops the threads. The applier is joined; the workers and
    /// the deleter share what is left of `wait`, counted from the call. Threads stuck in a filesystem call are left behind.
    pub fn shutdown(mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        self.send(Command::CancelAll);
        self.stop.store(true, SeqCst);
        let _ = self.cmd_tx.send(Command::CancelAll);
        if let Some(a) = self.applier.take() {
            let _ = a.join();
        }
        if let Some(p) = self.pool.take() {
            p.stop(deadline.saturating_duration_since(Instant::now()));
        }
        if let Some(l) = self.link.take() {
            l.close();
        }
        // The applier held the deleter's plan channel; with it gone the deleter finishes its
        // current item, syncs and closes the journal, and exits. Wait for that, in what is left
        // of `wait`.
        self.delete_abort.store(true, SeqCst);
        if let Some(d) = self.deleter.take() {
            while !d.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if d.is_finished() {
                let _ = d.join();
            }
        }
    }
}

/// Where an engine's tree comes from, for [`Engine::start_inner`].
enum Source {
    /// Scans on this host.
    Local,
    /// Scans by an agent on another host.
    Remote(Remote),
    /// A tree read from a file, never scanned.
    Imported(Box<Tree>),
}

/// The link to an agent.
struct Remote {
    link: Arc<crate::remote::Link>,
    reader: Box<dyn std::io::Read + Send>,
    measure_min: u64,
    privileged: bool,
}

/// The scan limits `opts` asks for, credit and pool sized for its workers where it leaves
/// them at 0.
fn scan_limits(opts: &EngineOptions) -> Limits {
    let mut limits = opts.limits.clone();
    if limits.chunk_credit == 0 {
        limits.chunk_credit = opts.workers.max(2);
    }
    if limits.pool_workers == 0 {
        limits.pool_workers = opts.workers.max(1);
    }
    limits
}

/// `rx`, or a channel that never receives when the pool that would send on it is off: a
/// receiver with no senders is always ready, and the applier's `select!` would return at once
/// and take the write lock in a loop.
fn or_never<T>(rx: Receiver<T>, on: bool) -> Receiver<T> {
    if on { rx } else { crossbeam_channel::never() }
}

#[cfg(test)]
mod tests {
    use super::EngineOptions;

    #[test]
    fn default_options_read_nothing_from_the_environment() {
        let opts = EngineOptions::default();
        assert_eq!(opts.journal_dir, None);
        assert_eq!(opts.crash_dir, None);
        assert!(!opts.check_invariants);
    }
}
