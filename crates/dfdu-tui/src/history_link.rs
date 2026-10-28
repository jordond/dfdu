//! The browser's link to the scan history (`dfdu_store::history`): the last scan's sizes, read
//! in the background for the growth notes, and the scan recorded once it is complete. As in
//! the CLI, problems with the history are reported (in the status line), never fatal.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use dfdu_engine::Engine;
use dfdu_store::history::{self, Growth};

use crate::App;

/// What the last scan's sizes came to: the sizes, `None` when there are none, or why they could
/// not be read.
pub type Loaded = Result<Option<Arc<Growth>>, String>;

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
    pub fn start(db: PathBuf, root: PathBuf, options: String) -> HistoryLink {
        let started_at = now_secs();
        let growth: Arc<OnceLock<Loaded>> = Arc::default();
        let (lock, path, opts) = (growth.clone(), db.clone(), options.clone());
        let spawned = thread::Builder::new()
            .name("dfdu-history".into())
            .spawn(move || {
                let g = Growth::load(&path, &root, &opts, Some(started_at));
                let _ = lock.set(g.map(|g| g.map(Arc::new)).map_err(|e| e.to_string()));
            });
        if let Err(e) = spawned {
            let _ = growth.set(Err(format!("cannot start a thread: {e}")));
        }
        HistoryLink {
            db,
            options,
            started_at,
            growth,
        }
    }

    /// How a problem with the history is shown, as the CLI words it.
    fn problem(&self, e: &str) -> String {
        format!("history {}: {e}", self.db.display())
    }
}

/// Records the scan once its first job is done, on a thread the drop waits for.
pub(crate) struct Recorder {
    link: Option<HistoryLink>,
    /// The load's outcome was taken in.
    loaded: bool,
    /// The record was started; the writer is kept until its outcome is taken in.
    started: bool,
    writer: Option<thread::JoinHandle<Result<(), String>>>,
}

impl Recorder {
    pub(crate) fn new(link: Option<HistoryLink>) -> Self {
        Recorder {
            link,
            loaded: false,
            started: false,
            writer: None,
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
                    app.growth = g.clone();
                    changed = g.is_some();
                }
                Err(e) => {
                    app.status = Some(link.problem(e));
                    changed = true;
                }
            }
        }
        if self.writer.as_ref().is_some_and(|w| w.is_finished())
            && let Some(w) = self.writer.take()
        {
            let result = w
                .join()
                .unwrap_or_else(|_| Err("the writer panicked".into()));
            if let Err(e) = result {
                app.status = Some(link.problem(&e));
                changed = true;
            }
        }
        changed
    }

    /// Starts the record once the scan is complete.
    fn start<B: dfdu_scan::Backend>(&mut self, engine: &Engine<B>) {
        let Some(link) = &self.link else {
            return;
        };
        let rec = engine.read(|m| {
            let done = m
                .status
                .jobs
                .first()
                .is_some_and(|j| j.phase == dfdu_engine::JobPhase::Done);
            if !done || m.status.busy() {
                return None;
            }
            let root = m.tree.roots().next()?;
            let t = m.tree.total(root);
            Some((
                m.tree.path_buf(root),
                history::rows(&m.tree, root),
                history::Sizes {
                    disk: t.alloc,
                    apparent: t.apparent,
                    entries: t.items,
                },
                t.errors,
                t.errors == 0 && !m.status.incomplete,
            ))
        });
        let Some((root, rows, total, errors, complete)) = rec else {
            return;
        };
        self.started = true;
        let link = link.clone();
        let spawned = thread::Builder::new()
            .name("dfdu-history".into())
            .spawn(move || {
                let mut h = history::History::open(&link.db).map_err(|e| e.to_string())?;
                h.record(&history::Record {
                    root: &root,
                    options: &link.options,
                    started_at: link.started_at,
                    finished_at: now_secs(),
                    complete,
                    total,
                    errors,
                    rows: &rows,
                })
                .map(drop)
                .map_err(|e| e.to_string())
            });
        self.writer = Some(spawned.unwrap_or_else(|e| {
            let e = format!("cannot start a thread: {e}");
            thread::spawn(move || Err(e))
        }));
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
            let _ = w.join();
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
            path: root,
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
}
