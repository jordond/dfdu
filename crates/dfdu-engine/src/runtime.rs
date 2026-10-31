//! The engine: its threads, the inbox the applier reads from, and the API over them.

mod applier_loop;
mod inbox;
mod journal;
pub use journal::Query as JournalQuery;
mod options;

pub(crate) use options::default_measurers;
pub use options::{EngineOptions, workers_for};

use dfdu_scan::panic_message;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use dfdu_clean::{DeleterMsg, spawn_deleter};
use dfdu_scan::runtime::Pool;
use dfdu_scan::{Backend, Budget, PortableBackend, Shared};
use dfdu_tree::{Host, HostPath, Tree};
use parking_lot::{Mutex, RwLock};

use crate::applier::{ApplierSettings, Command, Model};
use crate::classifier::{CheckMsg, spawn_classifiers};
use crate::crash::{CRASH_MODEL, record_applier_panic};
use crate::measure::{MeasureMsg, spawn_measurers};
use crate::threads::spawn;
use applier_loop::{Channels, applier_loop};
use inbox::Inbox;
use options::scan_limits;

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
    commands_sent: Arc<AtomicU64>,
    /// The resumes waiting for the change journal's answer ([`Engine::resume`]).
    asking: Mutex<Vec<journal::Asking>>,
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
    /// An engine on the portable backend with `workers` scan workers and otherwise the
    /// [`EngineOptions`] defaults, which read nothing from the environment: no deletion
    /// journal (deletes are refused) and no crash reports.
    #[must_use]
    pub fn portable(workers: usize) -> Self {
        Self::start(
            PortableBackend::default(),
            EngineOptions {
                workers,
                ..EngineOptions::default()
            },
        )
    }

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
    /// agent does not answer the greeting or speaks no common protocol version. The agent
    /// resolves `root` (as typed) on its host; the answer has it, to scan with
    /// [`Command::Scan`]. With `rules`, the agent classifies with them; the answer says what the
    /// scan root classifies against, to put in its options.
    ///
    /// # Errors
    ///
    /// If the agent does not greet back, speaks no common protocol version, or the link ends
    /// or carries something that is not a valid message before the greeting is over.
    pub fn start_remote(
        backend: B,
        opts: EngineOptions,
        io: crate::RemoteIo,
        root: &std::path::Path,
        rules: Option<crate::RemoteRules>,
    ) -> std::io::Result<(Self, crate::Connected)> {
        let (link, reader, connected) = crate::remote::connect(io, root, rules)?;
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
                // A file does not say which host its paths are on; its root's form does.
                let host = model
                    .tree
                    .roots()
                    .next()
                    .map(|r| Host::guess(model.tree.name(r)));
                model.tree.set_host(host.unwrap_or(Host::LOCAL));
                model.tree.bump_version();
                (None, true)
            }
        };
        let measure_min = remote
            .as_ref()
            .map_or(crate::applier::MEASURE_MIN, |r| r.measure_min);
        let privileged = remote
            .as_ref()
            .map_or_else(dfdu_sys::is_privileged, |r| r.privileged);
        let (link, reader) = remote.map(|r| (r.link, r.reader)).unzip();
        if let Some(l) = &link {
            model.tree.set_host(l.host);
        }
        let model = Arc::new(RwLock::new(model));
        *CRASH_MODEL.lock() = Some(Arc::downgrade(&model));
        let (cmd_tx, cmd_rx) = unbounded::<Command>();
        let (wake_tx, changes_rx) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));

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
        let measurers = match &link {
            Some(l) => (opts.measure_workers > 0 && measure_min > 0)
                .then(|| crate::remote::spawn_measurer(l.clone(), meas_tx.clone())),
            None => {
                (opts.measure_workers > 0).then(|| spawn_measurers(opts.measure_workers, &meas_tx))
            }
        };
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
        let settings = ApplierSettings {
            remote: link.clone(),
            read_only: opts.read_only,
            imported,
            measure: measurers.is_some(),
            measure_min,
            privileged,
            checks: classifiers.is_some(),
        };
        let (m, sh, st) = (model.clone(), shared.clone(), stop.clone());
        let chans = Channels {
            cmd_rx,
            inbox_rx,
            budget,
            del_rx,
            plans: deleter.reqs,
            meas_rx,
            measure: measurers,
            chk_rx,
            classify: classifiers,
            wake_tx,
        };
        let wake = chans.wake_tx.clone();
        let applier = spawn("dfdu-applier", move || {
            let crash_dir = opts.crash_dir.clone();
            let model = m.clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                applier_loop(
                    &m,
                    sh,
                    settings,
                    chans,
                    &st,
                    opts.auto_abandon,
                    opts.check_invariants,
                );
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
            commands_sent: Arc::new(AtomicU64::new(0)),
            asking: Mutex::new(Vec::new()),
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
        // applier never does I/O and deletion can require absolute paths inside the root. A
        // remote engine's roots are the agent's, resolved there
        // ([`Connected::root`](crate::Connected::root)).
        let cmd = match cmd {
            Command::Scan { path, opts } if self.link.is_none() && path.host() == Host::LOCAL => {
                let path = HostPath::local(&resolve_root(path.to_local()));
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

    /// Reads the model from another thread: the function returned runs the one it is given
    /// under the read lock, as [`Engine::read`] does. Do no I/O there, and keep it short: the
    /// applier and the browser wait for the lock.
    pub fn reader(&self) -> impl Fn(&mut dyn FnMut(&Model)) + Send + Sync + 'static {
        let model = self.model.clone();
        move |f| f(&model.read())
    }

    /// Replaces the tree with one read from a file, before anything is scanned, to bring it up
    /// to date with [`Command::Resume`]. A tree that is only browsed is started with
    /// [`Engine::start_imported`] instead.
    pub fn load(&self, tree: Tree) {
        let mut m = self.model.write();
        m.tree = tree;
        m.tree.bump_version();
    }

    /// Receives a message whenever the model changed.
    pub fn changes(&self) -> &Receiver<()> {
        &self.changes_rx
    }

    /// The local scan pool: `None` for an engine scanning through an agent, whose jobs run
    /// there (the wire carries no focus, and the agent keeps its own counters).
    pub fn local_scanner(&self) -> Option<&Shared<B>> {
        self.link.is_none().then_some(&*self.shared)
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

    /// Cancels everything and stops the threads. The applier is joined; the workers, the
    /// deleter and a change journal query of a resume share what is left of `wait`, counted
    /// from the call. Threads stuck in a filesystem call are left behind.
    pub fn shutdown(mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        // A resume the journal has not answered yet is not sent.
        let mut asking = std::mem::take(&mut *self.asking.lock());
        for a in &asking {
            a.cancel();
        }
        self.send(Command::CancelAll);
        self.stop.store(true, SeqCst);
        // Wakes the applier's `select!` to see `stop` without waiting out its timeout; not
        // counted in `commands_sent`, as the applier may stop before applying it.
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
        for a in &mut asking {
            a.stop(deadline);
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

/// `path` made absolute and canonical, as scan roots are: on Windows the plain `C:\` form
/// where it is equivalent (dunce), instead of the `\\?\` verbatim form std returns. Absolute
/// but not canonical when it cannot be resolved (it does not exist), and `path` itself when
/// it cannot be made absolute either (an empty path, or no current directory): the scan of
/// such a root fails, and deletion, which requires absolute paths, refuses inside it.
pub(crate) fn resolve_root(path: PathBuf) -> PathBuf {
    dunce::canonicalize(&path)
        .or_else(|_| std::path::absolute(&path))
        .unwrap_or(path)
}

/// `rx`, or a channel that never receives when the pool that would send on it is off: a
/// receiver with no senders is always ready, and the applier's `select!` would return at once
/// and take the write lock in a loop.
fn or_never<T>(rx: Receiver<T>, on: bool) -> Receiver<T> {
    if on { rx } else { crossbeam_channel::never() }
}
