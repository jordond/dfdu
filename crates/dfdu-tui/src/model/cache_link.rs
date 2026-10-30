//! The browser's link to the scan cache (`dfdu_store::cache`): the cached scan of the directory
//! opened, read in the background and shown while it is brought up to date, and complete scans
//! saved in the background once they end. As with the history, problems are reported in the
//! status line and never fatal: the directory is scanned instead.
//!
//! Nothing is saved on the way out: quitting cancels a save still running (the file cached
//! before stays) and waits at most [`CANCEL_WAIT`] for it to stop.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, JobPhase, JournalPosition, Model, WaitIdle};
use dfdu_scan::JobOptions;
use dfdu_store::cache::{self, Hit, MIN_SCAN_MS, Saved, Slot};
use dfdu_store::snapshot::Header;
use dfdu_tree::DirState;

use crate::App;

/// How long the tree must stay the same after it changed (a delete, a rescan) before it is
/// saved again.
const RESAVE_QUIET: Duration = Duration::from_secs(10);
/// How long quitting waits for a cancelled save to stop.
const CANCEL_WAIT: Duration = Duration::from_millis(500);

/// Where the scan of the directory opened is cached, and what to scan when nothing is.
#[derive(Clone, Debug)]
pub struct CacheLink {
    pub slot: Slot,
    /// The directory as given, scanned when nothing is cached.
    pub path: PathBuf,
    /// The options of its scans.
    pub job: JobOptions,
    /// Save complete scans; off for a read-only browser.
    pub save: bool,
}

/// What the header says about the cached scan shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cached {
    /// It is being read.
    Loading,
    /// It is shown, written at `written` (seconds since the Unix epoch), and being brought up
    /// to date.
    Refreshing { written: u64 },
}

/// How long ago `then` was at `now` (seconds since the Unix epoch), in a few cells: `40 s`,
/// `12 min`, `3 h`, `5 d`.
#[must_use]
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => format!("{s} s"),
        60..3_600 => format!("{} min", s / 60),
        3_600..86_400 => format!("{} h", s / 3_600),
        _ => format!("{} d", s / 86_400),
    }
}

/// A cached scan read, ready to resume: and where the change journal stood just before, for the
/// next save.
struct Prepared {
    hit: Hit,
    position: Option<JournalPosition>,
}

/// What the loader thread ends with: the scan, `None` when there is none to use, or why it
/// could not be read.
type Loaded = Result<Option<Prepared>, String>;

/// The scan or resume whose result is saved.
struct Run {
    started: Instant,
    started_secs: u64,
    /// A resume from the change journal keeps the cached scan's last full scan and its
    /// duration; a full scan replaces them.
    kept: Option<(u64, u64)>,
    /// Where the change journal stood before it started.
    journal: Option<JournalPosition>,
    /// Once it ended: the last full scan and its duration to save, when it is worth saving.
    saved_as: Option<Option<(u64, u64)>>,
}

/// A save running on its thread.
struct Writer {
    thread: thread::JoinHandle<std::io::Result<Saved>>,
    cancel: Arc<AtomicBool>,
    /// Disconnected once the thread is done: it holds the other end until then.
    done: mpsc::Receiver<()>,
}

/// Loads the cached scan, scans when there is none, and saves complete scans, on threads the
/// drop stops.
pub(crate) struct Cacher {
    link: Option<CacheLink>,
    started: bool,
    loader: Option<thread::JoinHandle<Loaded>>,
    run: Option<Run>,
    writer: Option<Writer>,
    /// The tree version last saved, or being saved.
    saved: Option<u64>,
    /// The version the tree changed to since, and when it was first seen.
    changed: Option<(u64, Instant)>,
    /// Scans quicker than this are not saved ([`MIN_SCAN_MS`]).
    min_scan_ms: u64,
    /// Where a directory's change journal stands ([`dfdu_engine::journal_position`]).
    position: fn(&Path) -> Option<JournalPosition>,
    /// What changed below a directory since a position
    /// ([`dfdu_engine::changed_since_cancellable`]).
    changed_since: dfdu_engine::JournalQuery,
}

impl Cacher {
    pub(crate) fn new(link: Option<CacheLink>) -> Self {
        Cacher {
            link,
            started: false,
            loader: None,
            run: None,
            writer: None,
            saved: None,
            changed: None,
            min_scan_ms: MIN_SCAN_MS,
            position: dfdu_engine::journal_position,
            changed_since: dfdu_engine::changed_since_cancellable,
        }
    }

    /// Starts on the first call: loads the cached scan, or scans. Then takes in the load, the
    /// end of the scan or resume (and saves it), and the end of a save, reporting problems in
    /// the status line. True when there is something new to draw.
    pub(crate) fn poll<B: dfdu_scan::Backend>(
        &mut self,
        app: &mut App,
        engine: &Engine<B>,
    ) -> bool {
        let Some(link) = self.link.clone() else {
            return false;
        };
        if !self.started {
            self.started = true;
            self.begin(&link, app, engine);
            return true;
        }
        let mut changed = false;
        if self
            .loader
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
            && let Some(l) = self.loader.take()
        {
            let loaded = l
                .join()
                .unwrap_or_else(|_| Err("the loader panicked".into()));
            self.loaded(&link, loaded, app, engine);
            changed = true;
        }
        changed |= self.run_ended(&link, app, engine);
        changed |= self.save_ended(&link, app);
        self.resave(&link, engine);
        changed
    }

    /// Reads the cached scan on a thread when there is one; scans otherwise.
    fn begin<B: dfdu_scan::Backend>(
        &mut self,
        link: &CacheLink,
        app: &mut App,
        engine: &Engine<B>,
    ) {
        let now = cache::now();
        if link.slot.cached(now) {
            let slot = link.slot.clone();
            let root = link.slot.key.root().to_path_buf();
            let journal_position = self.position;
            let spawned = thread::Builder::new()
                .name("dfdu-cache".into())
                .spawn(move || {
                    let hit = slot.load(now).map_err(|e| e.to_string())?;
                    slot.sweep(now);
                    // Before the resume asks the journal what changed: what changes from here
                    // on is asked about again next time.
                    let position = hit.as_ref().and_then(|_| journal_position(&root));
                    Ok(hit.map(|hit| Prepared { hit, position }))
                });
            if let Ok(t) = spawned {
                self.loader = Some(t);
                app.cached = Some(Cached::Loading);
                return;
            }
        } else {
            let slot = link.slot.clone();
            let _ = thread::Builder::new()
                .name("dfdu-cache".into())
                .spawn(move || slot.sweep(now));
        }
        self.scan(link, engine);
    }

    /// Scans the directory.
    fn scan<B: dfdu_scan::Backend>(&mut self, link: &CacheLink, engine: &Engine<B>) {
        // Taken before the scan, so that what changes during it is replayed on resume.
        let journal = (self.position)(link.slot.key.root());
        self.run = Some(Run::new(None, journal));
        engine.send(Command::Scan {
            path: link.path.clone().into(),
            opts: link.job.clone(),
        });
    }

    /// Takes in what the loader read: shows the cached scan and resumes it, or scans.
    fn loaded<B: dfdu_scan::Backend>(
        &mut self,
        link: &CacheLink,
        loaded: Loaded,
        app: &mut App,
        engine: &Engine<B>,
    ) {
        match loaded {
            Ok(Some(Prepared { hit, position })) => {
                let h = &hit.loaded.header;
                // Rescanned whole when the last full scan is old: asking the journal would
                // keep what it missed.
                let journal = h.journal.filter(|_| !hit.full_rescan);
                let kept = journal.map(|_| (h.full_scan_at(), h.scan_ms));
                app.cached = Some(Cached::Refreshing { written: h.written });
                self.run = Some(Run::new(kept, position));
                engine.resume_with(
                    hit.loaded.tree,
                    journal,
                    link.job.clone(),
                    self.changed_since,
                );
            }
            Ok(None) => {
                app.cached = None;
                self.scan(link, engine);
            }
            Err(e) => {
                app.cached = None;
                app.status = Some(format!(
                    "cached scan {}: {e}; scanning",
                    link.slot.path.display()
                ));
                self.scan(link, engine);
            }
        }
    }

    /// Notes the end of the scan or resume, and saves it when it is complete and was worth it.
    fn run_ended<B: dfdu_scan::Backend>(
        &mut self,
        link: &CacheLink,
        app: &mut App,
        engine: &Engine<B>,
    ) -> bool {
        let Some(run) = self.run.as_mut().filter(|r| r.saved_as.is_none()) else {
            return false;
        };
        let Some((complete, version)) = ended(engine) else {
            return false;
        };
        let ms = u64::try_from(run.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (last_full_scan, scan_ms) = run.kept.unwrap_or((run.started_secs, ms));
        let worth = complete && link.save && scan_ms >= self.min_scan_ms;
        run.saved_as = Some(worth.then_some((last_full_scan, scan_ms)));
        let shown = app.cached.take().is_some();
        if worth {
            self.save(link, engine, version);
        }
        shown
    }

    /// Saves the tree, at `version`, on a thread.
    fn save<B: dfdu_scan::Backend>(&mut self, link: &CacheLink, engine: &Engine<B>, version: u64) {
        let Some(run) = &self.run else {
            return;
        };
        let Some(Some((last_full_scan, scan_ms))) = run.saved_as else {
            return;
        };
        let now = cache::now();
        let header = Header {
            tool_version: env!("CARGO_PKG_VERSION").into(),
            written: now,
            cross_mounts: link.slot.key.cross_mounts,
            network: link.slot.key.network,
            journal: run.journal,
            last_full_scan,
            scan_ms,
        };
        let read = engine.reader();
        let cancel = Arc::new(AtomicBool::new(false));
        let (slot, stop) = (link.slot.clone(), cancel.clone());
        let (running, done) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("dfdu-cache".into())
            .spawn(move || {
                let _running: mpsc::Sender<()> = running;
                let saved = slot.save(&header, |f| read(&mut |m| f(&m.tree)), &stop)?;
                if matches!(saved, Saved::Written { .. }) {
                    slot.sweep(now);
                }
                Ok(saved)
            });
        if let Ok(thread) = spawned {
            self.writer = Some(Writer {
                thread,
                cancel,
                done,
            });
            self.saved = Some(version);
            self.changed = None;
        }
    }

    /// Takes in the end of a save: a problem goes in the status line, and a tree that changed
    /// while it was written is saved again once it stays the same.
    fn save_ended(&mut self, link: &CacheLink, app: &mut App) -> bool {
        if !self.writer.as_ref().is_some_and(|w| w.thread.is_finished()) {
            return false;
        }
        let Some(w) = self.writer.take() else {
            return false;
        };
        match w.thread.join() {
            Ok(Ok(Saved::Written { .. } | Saved::Cancelled)) => false,
            Ok(Ok(Saved::Changed)) => {
                self.saved = None;
                false
            }
            Ok(Err(e)) => {
                app.status = Some(format!("cache {}: {e}", link.slot.path.display()));
                true
            }
            Err(_) => {
                app.status = Some(format!(
                    "cache {}: the writer panicked",
                    link.slot.path.display()
                ));
                true
            }
        }
    }

    /// Saves again a tree that changed since it was saved, once it has stayed the same for
    /// [`RESAVE_QUIET`] and is still complete.
    fn resave<B: dfdu_scan::Backend>(&mut self, link: &CacheLink, engine: &Engine<B>) {
        let worth = self
            .run
            .as_ref()
            .is_some_and(|r| matches!(r.saved_as, Some(Some(_))));
        if !worth || self.writer.is_some() {
            return;
        }
        let Some((true, version)) = ended(engine) else {
            self.changed = None;
            return;
        };
        if self.saved == Some(version) {
            self.changed = None;
            return;
        }
        match self.changed {
            Some((v, since)) if v == version => {
                if since.elapsed() >= RESAVE_QUIET {
                    self.save(link, engine, version);
                }
            }
            _ => self.changed = Some((version, Instant::now())),
        }
    }

    /// Something is being loaded, saved or waited for, so [`Cacher::poll`] has more to take
    /// in.
    pub(crate) fn busy(&self) -> bool {
        self.link.is_some()
            && (!self.started
                || self.loader.is_some()
                || self.writer.is_some()
                || self.changed.is_some())
    }
}

impl Run {
    fn new(kept: Option<(u64, u64)>, journal: Option<JournalPosition>) -> Run {
        Run {
            started: Instant::now(),
            started_secs: cache::now(),
            kept,
            journal,
            saved_as: None,
        }
    }
}

/// Whether the scan or resume ended with the tree complete, and the tree's version: `None` while
/// it runs, and while a command sent is not applied yet. A resume is sent once the change
/// journal answered, and a rescan or a mount entered meanwhile ends first: what it leaves is the
/// cached tree, not yet brought up to date.
fn ended<B: dfdu_scan::Backend>(engine: &Engine<B>) -> Option<(bool, u64)> {
    if engine.wait_idle(Some(Duration::ZERO)) == WaitIdle::TimedOut {
        return None;
    }
    engine.read(|m| finished(m).map(|c| (c, m.tree.version())))
}

/// Whether the scan or resume ended with the tree complete: `None` while it runs.
fn finished(m: &Model) -> Option<bool> {
    let done = m
        .status
        .jobs
        .first()
        .is_some_and(|j| j.phase == JobPhase::Done);
    if !done || m.status.busy() {
        return None;
    }
    let root = m.tree.roots().next()?;
    let state = m.tree.dir(root).map(|d| d.state);
    Some(
        !m.status.incomplete
            && m.status.disconnected.is_none()
            && m.status.fatal.is_none()
            && state == Some(DirState::Complete),
    )
}

impl Drop for Cacher {
    fn drop(&mut self) {
        let Some(w) = self.writer.take() else {
            return;
        };
        w.cancel.store(true, Relaxed);
        if w.done.recv_timeout(CANCEL_WAIT) == Err(RecvTimeoutError::Disconnected) {
            let _ = w.thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
