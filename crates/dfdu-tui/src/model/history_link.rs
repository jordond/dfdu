//! The browser's link to the scan history (`dfdu_store::history`): the last scan's sizes, read
//! in the background for the growth notes, and the scan recorded once it is complete. As in
//! the CLI, problems with the history are reported (in the status line), never fatal.
//!
//! Quitting waits at most [`QUIT_WAIT`] for a record still being written. One cut off when the
//! process ends is rolled back, as it is written in one transaction.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use dfdu_engine::{Engine, WaitIdle};
use dfdu_store::history::{self, Growth, Scanned};
use dfdu_tree::now_secs;

use crate::app::App;

/// How long quitting waits for the record to be written. It cannot be cancelled, and another
/// dfdu writing the history can hold it up for seconds.
const QUIT_WAIT: Duration = Duration::from_secs(2);

/// What the last scan's sizes came to: the sizes, `None` when there are none, or why they could
/// not be read.
pub type Loaded = io::Result<Option<Arc<Growth>>>;

/// The browser's link to the scan history: the last scan's sizes, loaded in the background, and
/// what to record the scan under when it finishes.
#[derive(Clone, Debug)]
pub struct HistoryLink {
    pub db: PathBuf,
    pub options: String,
    pub started_at: i64,
    /// Set once the load ends.
    pub growth: Arc<OnceLock<Loaded>>,
}

impl HistoryLink {
    /// Starts loading the last scan of `root` with `options` from `db`.
    #[must_use]
    pub fn start(db: PathBuf, root: PathBuf, options: String) -> HistoryLink {
        let started_at = now_secs();
        let growth: Arc<OnceLock<Loaded>> = Arc::default();
        let (lock, path, opts) = (growth.clone(), db.clone(), options.clone());
        let spawned = thread::Builder::new()
            .name("dfdu-history".into())
            .spawn(move || {
                let g = Growth::load(&path, &root, &opts, Some(started_at));
                let _ = lock.set(g.map(|g| g.map(Arc::new)));
            });
        if let Err(e) = spawned {
            let _ = growth.set(Err(thread_error(&e)));
        }
        HistoryLink {
            db,
            options,
            started_at,
            growth,
        }
    }

    /// How a problem with the history is shown, as the CLI words it.
    fn problem(&self, e: &io::Error) -> String {
        format!("history {}: {e}", self.db.display())
    }
}

/// Why a history thread could not be started, as the status line says it.
fn thread_error(e: &io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("cannot start a thread: {e}"))
}

/// The record being written on its thread.
struct Writer {
    thread: thread::JoinHandle<io::Result<()>>,
    /// Disconnected once the thread is done: it holds the other end until then.
    done: crossbeam_channel::Receiver<()>,
}

impl Writer {
    /// Waits at most `wait` for the thread to end, and joins it if it did.
    fn finish(self, wait: Duration) {
        if self.done.recv_timeout(wait) == Err(RecvTimeoutError::Disconnected) {
            let _ = self.thread.join();
        }
    }
}

/// Records the scan once its first job is done, on a thread the drop waits a bounded time for.
pub(crate) struct Recorder {
    link: Option<HistoryLink>,
    /// The load's outcome was taken in.
    loaded: bool,
    /// The record was started; the writer is kept until its outcome is taken in.
    started: bool,
    writer: Option<Writer>,
    /// The writer could not be started: reported by the next poll.
    unstarted: Option<io::Error>,
}

impl Recorder {
    pub(crate) fn new(link: Option<HistoryLink>) -> Self {
        Recorder {
            link,
            loaded: false,
            started: false,
            writer: None,
            unstarted: None,
        }
    }

    /// Takes in what the history has: the last scan's sizes once loaded, and records this scan
    /// once it is complete. A problem with either is put in the status line. True when there
    /// is something new to draw.
    pub(crate) fn poll<B: dfdu_scan::Backend>(
        &mut self,
        app: &mut App,
        engine: &Engine<B>,
    ) -> bool {
        if !self.started {
            self.start(engine);
        }
        let Some(link) = &self.link else {
            return false;
        };
        let mut changed = false;
        if !self.loaded
            && let Some(loaded) = link.growth.get()
        {
            self.loaded = true;
            match loaded {
                Ok(g) => {
                    app.growth.clone_from(g);
                    changed = g.is_some();
                }
                Err(e) => {
                    app.status = Some(link.problem(e));
                    changed = true;
                }
            }
        }
        if let Some(w) = self.writer.take_if(|w| w.thread.is_finished()) {
            let result = w
                .thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the writer panicked")));
            if let Err(e) = result {
                app.status = Some(link.problem(&e));
                changed = true;
            }
        }
        if let Some(e) = self.unstarted.take() {
            app.status = Some(link.problem(&e));
            changed = true;
        }
        changed
    }

    /// Starts the record once the scan is complete.
    fn start<B: dfdu_scan::Backend>(&mut self, engine: &Engine<B>) {
        let Some(link) = &self.link else {
            return;
        };
        // A resume waiting for the change journal is not applied yet, and a rescan sent
        // meanwhile ends before it.
        if engine.wait_idle(Some(Duration::ZERO)) == WaitIdle::TimedOut {
            return;
        }
        let scan = engine.read(|m| {
            let done = m
                .status
                .jobs
                .first()
                .is_some_and(|j| j.phase == dfdu_engine::JobPhase::Done);
            if !done || m.status.busy() {
                return None;
            }
            let root = m.tree.roots().next()?;
            Some(Scanned::read(&m.tree, root, m.complete(root)))
        });
        let Some(scan) = scan else {
            return;
        };
        self.started = true;
        let link = link.clone();
        let (running, done) = crossbeam_channel::bounded(0);
        let spawned = thread::Builder::new()
            .name("dfdu-history".into())
            .spawn(move || {
                let _running: crossbeam_channel::Sender<()> = running;
                let mut h = history::History::open(&link.db)?;
                h.record(&scan.record(&link.options, link.started_at, now_secs()))
                    .map(drop)
            });
        match spawned {
            Ok(thread) => self.writer = Some(Writer { thread, done }),
            Err(e) => self.unstarted = Some(thread_error(&e)),
        }
    }

    /// The history is still being read or written, so [`Recorder::poll`] has more to take in.
    pub(crate) fn busy(&self) -> bool {
        self.link
            .as_ref()
            .is_some_and(|l| l.growth.get().is_none() || self.writer.is_some())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(w) = self.writer.take() {
            w.finish(QUIT_WAIT);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dfdu_engine::EngineOptions;
    use dfdu_scan::PortableBackend;

    use super::*;
    use crate::theme::Theme;

    fn poll_until(rec: &mut Recorder, app: &mut App, engine: &Engine<PortableBackend>) -> bool {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if rec.poll(app, engine) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// A history that cannot be opened is reported in the status line, not taken for one
    /// without a previous scan.
    #[test]
    fn load_errors_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory where the database should be: it exists, but cannot be opened.
        let db = tmp.path().join("history.db");
        std::fs::create_dir(&db).expect("mkdir");
        let link = HistoryLink::start(db.clone(), tmp.path().into(), "k".into());
        let engine = Engine::start(PortableBackend::default(), EngineOptions::default());
        let mut app = App::new(Theme::no_color(), std::time::Instant::now());
        let mut rec = Recorder::new(Some(link));
        assert!(poll_until(&mut rec, &mut app, &engine));
        let status = app.status.clone().expect("a status");
        assert!(
            status.starts_with(&format!("history {}: ", db.display())),
            "{status}"
        );
        assert!(app.growth.is_none());
        engine.shutdown(Duration::from_millis(200));
    }

    /// A scan that cannot be recorded is reported in the status line once the writer ends.
    #[test]
    fn record_errors_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("scanned");
        std::fs::create_dir(&root).expect("mkdir");
        std::fs::write(root.join("a"), b"data").expect("write");
        // The database's directory is a file: there is no history to load, and none to write.
        let parent = tmp.path().join("state");
        std::fs::write(&parent, b"").expect("write");
        let db = parent.join("history.db");
        let link = HistoryLink::start(db.clone(), root.clone(), "k".into());
        let engine = Engine::start(PortableBackend::default(), EngineOptions::default());
        engine.send(dfdu_engine::Command::Scan {
            path: root.into(),
            opts: dfdu_scan::JobOptions::default(),
        });
        assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
        let mut app = App::new(Theme::no_color(), std::time::Instant::now());
        let mut rec = Recorder::new(Some(link));
        assert!(poll_until(&mut rec, &mut app, &engine));
        let status = app.status.clone().expect("a status");
        assert!(
            status.starts_with(&format!("history {}: ", db.display())),
            "{status}"
        );
        assert!(!rec.busy());
        engine.shutdown(Duration::from_millis(200));
    }

    fn writer(f: impl FnOnce() + Send + 'static) -> Writer {
        let (running, done) = crossbeam_channel::bounded(0);
        let thread = thread::spawn(move || {
            let _running: crossbeam_channel::Sender<()> = running;
            f();
            Ok(())
        });
        Writer { thread, done }
    }

    /// Quitting stops waiting for a record held up, and joins one that ends.
    #[test]
    fn the_writer_is_waited_for_a_bounded_time() {
        let wait = Duration::from_millis(50);
        let (release, held) = crossbeam_channel::bounded::<()>(0);
        let t0 = std::time::Instant::now();
        writer(move || {
            let _ = held.recv();
        })
        .finish(wait);
        let waited = t0.elapsed();
        assert!(waited >= wait, "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        drop(release);

        let (ended, end) = crossbeam_channel::bounded(1);
        writer(move || {
            let _ = ended.send(());
        })
        .finish(Duration::from_secs(5));
        assert!(end.try_recv().is_ok(), "joined after the thread ran");
        let mut rec = Recorder::new(None);
        rec.writer = Some(writer(|| ()));
        drop(rec);
    }
}
