//! The session driver ([`Session`]): the engine, the state of a browser and the jobs that run
//! beside them (the scan history, the scan cache, the lookups outside the scan). A front end's
//! loop waits on input, on [`Session::changes`] and until [`Tick::wake_at`], calls
//! [`Session::tick`] and shows a frame when it says something changed.
//!
//! A tick takes in what the history, the cache and the lookups found, then reads the model when
//! due: a snapshot of the directory when scan data came in (at most every `DATA_FRAME`, and at
//! most every nine times the cost of the last read, so the read lock is held at most a tenth of
//! the time) or when the user opened another directory or changed what a snapshot counts, and
//! otherwise only what the features read beside it when they need it sooner
//! ([`crate::extras::stale`]). Without a read it applies the sorts asked for and the automatic
//! re-sort once its deadline passed. It asks the scanner to scan the directory shown first.
//!
//! Time is passed in (`now`), so the pacing is the caller's clock.
//!
//! A front end keeps its own view state next to the [`SessionState`] it shows: the session
//! holds both as a [`FrontEnd`], which also says how the front end follows a new snapshot.

mod pacing;
#[cfg(test)]
mod tests;

use std::io;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use dfdu_engine::Engine;
use dfdu_scan::Backend;

use crate::cache_link::{CacheLink, Cacher};
use crate::debug;
use crate::effect::{self, Effect, Output};
use crate::extras::{self, Applied, Read};
use crate::history_link::{HistoryLink, Recorder};
use crate::snapshot::{self, Snapshot};
use crate::state::SessionState;
use pacing::Pacing;
pub use pacing::{DATA_FRAME, spinning};

/// What a front end keeps around the [`SessionState`] it shows, and how it follows the rows.
/// [`SessionState`] itself is one that keeps nothing else.
pub trait FrontEnd {
    fn state(&self) -> &SessionState;

    fn state_mut(&mut self) -> &mut SessionState;

    /// Folds in `snap`, read at `now` ([`SessionState::refresh`]). A front end that shows rows
    /// moving or keeps deleted rows in place on screen does that here, and follows the
    /// [`Reordered`](crate::state::Reordered) it returns with its scroll.
    fn refresh(&mut self, snap: Snapshot, now: Instant) {
        let _ = self.state_mut().refresh(snap, now, false);
    }

    /// Applies the sorts asked for and the automatic sort at `now`
    /// ([`SessionState::reorder`]), as [`FrontEnd::refresh`] does with a snapshot.
    fn reorder(&mut self, now: Instant) {
        let _ = self.state_mut().reorder(now, false);
    }

    /// Follows what taking in a read changed ([`extras::apply`]).
    fn applied(&mut self, _applied: Applied) {}

    /// The front end shows the breakdown of the directory by file type (the type picker is
    /// open), so a snapshot is read with it.
    fn breakdown(&self) -> bool {
        false
    }
}

impl FrontEnd for SessionState {
    fn state(&self) -> &SessionState {
        self
    }

    fn state_mut(&mut self) -> &mut SessionState {
        self
    }
}

/// The jobs a session runs beside the engine.
#[derive(Clone, Debug, Default)]
pub struct SessionOptions {
    /// The scan history the scan is recorded in, with the last scan's sizes loading.
    pub history: Option<HistoryLink>,
    /// The scan cache: the session opens it (sending the first scan or resume itself) and saves
    /// complete scans to it.
    pub cache: Option<CacheLink>,
}

/// What a tick found.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[must_use]
pub struct Tick {
    /// Something shown changed, or a spinner is due to move: show a frame.
    pub changed: bool,
    /// When to tick again, `None` when nothing is due until the engine changes or the user
    /// acts. It counts a frame shown now when `changed`.
    pub wake_at: Option<Instant>,
}

/// The engine, owned by the session or by its caller.
enum Held<'e, B: Backend> {
    Owned(Engine<B>),
    Borrowed(&'e Engine<B>),
}

impl<B: Backend> Held<'_, B> {
    fn get(&self) -> &Engine<B> {
        match self {
            Held::Owned(e) => e,
            Held::Borrowed(e) => e,
        }
    }
}

/// The engine, the state of a browser (inside the front end's `F`) and the jobs beside them.
pub struct Session<'e, B: Backend, F: FrontEnd = SessionState> {
    engine: Held<'e, B>,
    front: F,
    pacing: Pacing,
    // Dropped in this order: a save to the cache is cancelled before a record in the history
    // is waited for.
    cacher: Cacher,
    recorder: Recorder,
}

impl<B: Backend, F: FrontEnd> Session<'static, B, F> {
    /// A session that owns `engine` and shuts it down with itself ([`Session::shutdown`]).
    pub fn new(engine: Engine<B>, front: F, opts: SessionOptions) -> Self {
        Session::with(Held::Owned(engine), front, opts)
    }
}

impl<'e, B: Backend, F: FrontEnd> Session<'e, B, F> {
    /// A session on `engine`, which its caller shuts down.
    pub fn over(engine: &'e Engine<B>, front: F, opts: SessionOptions) -> Self {
        Session::with(Held::Borrowed(engine), front, opts)
    }

    fn with(engine: Held<'e, B>, front: F, opts: SessionOptions) -> Self {
        Session {
            engine,
            pacing: Pacing::new(front.state().size_mode),
            front,
            cacher: Cacher::new(opts.cache),
            recorder: Recorder::new(opts.history),
        }
    }

    pub fn engine(&self) -> &Engine<B> {
        self.engine.get()
    }

    pub fn state(&self) -> &SessionState {
        self.front.state()
    }

    pub fn state_mut(&mut self) -> &mut SessionState {
        self.front.state_mut()
    }

    /// The front end's state around the session's.
    pub fn front(&self) -> &F {
        &self.front
    }

    pub fn front_mut(&mut self) -> &mut F {
        &mut self.front
    }

    /// Receives a message whenever the engine's model changed, for a front end that waits on
    /// it. The next tick takes in the messages left in it; a front end that took one calls
    /// [`Session::note_change`].
    pub fn changes(&self) -> &Receiver<()> {
        self.engine.get().changes()
    }

    /// Takes in a message the front end took from [`Session::changes`]. The engine reports a
    /// change only when its fingerprint changed, but it can still be what the last read saw,
    /// so it is compared first.
    pub fn note_change(&mut self) {
        let v = self.engine.get().read(dfdu_engine::Model::fingerprint);
        if Some(v) != self.pacing.seen_version {
            self.pacing.data_dirty = true;
        }
    }

    /// Takes in what the history, the cache and the lookups outside the scan found, reads the
    /// model when due and sorts the rows when due, at `now`.
    ///
    /// # Errors
    ///
    /// When the engine stopped with a fatal error: showing its stale data as if it were live
    /// would mislead.
    pub fn tick(&mut self, now: Instant) -> io::Result<Tick> {
        if self.changes().try_iter().count() > 0 {
            self.note_change();
        }
        let polled = self.poll();
        let s = self.front.state();
        let read = self.pacing.read_due(s, self.front.breakdown(), now);
        let sort_due = s.next_sort_at().is_some_and(|t| now >= t);
        let spinner_due = self.pacing.spinner_due(s, now);
        match read {
            Read::Full => self.read_full(now)?,
            Read::Extras => self.read_extras(now),
            Read::Nothing => self.front.reorder(now),
        }
        let changed = polled || read != Read::Nothing || sort_due || spinner_due;
        let wake_at = self
            .pacing
            .wake_at(self.front.state(), changed, self.polling(), now);
        Ok(Tick { changed, wake_at })
    }

    /// Notes a frame the front end showed at `now`: spinners move from it.
    pub fn shown(&mut self, now: Instant) {
        self.pacing.shown(self.front.state(), now);
    }

    /// When to tick again, as [`Tick::wake_at`], after the frames noted so far.
    pub fn wake_at(&self, now: Instant) -> Option<Instant> {
        (self.pacing).wake_at(self.front.state(), false, self.polling(), now)
    }

    /// How long the last read of the model held the lock.
    pub fn read_cost(&self) -> Duration {
        self.pacing.read_cost
    }

    /// A spinner or a clock on screen moves ([`spinning`]).
    pub fn spinning(&self) -> bool {
        spinning(self.front.state())
    }

    /// Carries out `fx` ([`effect::carry_out`]) and returns what only the front end can do.
    pub fn carry_out(&mut self, fx: Vec<Effect>) -> Vec<Output> {
        effect::carry_out(self.front.state_mut(), self.engine.get(), fx)
    }

    /// Stops what runs beside the engine: stops a comparison of duplicates, cancels a save to the
    /// cache, waits a bounded time for a record in the history, and shuts the engine down when
    /// the session owns it, waiting at most `wait` ([`Engine::shutdown`]).
    pub fn shutdown(self, wait: Duration) {
        let Session {
            engine,
            mut front,
            pacing: _,
            cacher,
            recorder,
        } = self;
        let _ = crate::dupes::cancel(front.state_mut());
        drop(cacher);
        drop(recorder);
        drop(front);
        if let Held::Owned(e) = engine {
            e.shutdown(wait);
        }
    }

    /// Polls the history, the cache and the lookups: true when one of them has something new
    /// to show.
    fn poll(&mut self) -> bool {
        let engine = self.engine.get();
        let s = self.front.state_mut();
        let history = self.recorder.poll_into(s, engine);
        let cache = self.cacher.poll(s, engine);
        let outside = s.outside.poll();
        history || cache || outside
    }

    /// The history or the cache has more to take in.
    fn polling(&self) -> bool {
        self.recorder.busy() || self.cacher.busy()
    }

    /// Reads a new snapshot and the extras, and folds them in.
    fn read_full(&mut self, now: Instant) -> io::Result<()> {
        let engine = self.engine.get();
        let s = self.front.state();
        let mut target = s.target.clone();
        let mode = s.size_mode;
        let breakdown = self.front.breakdown();
        let (snap, extras, cost) = engine.read(|m| {
            let t = Instant::now();
            let snap =
                snapshot::build_view(m, &mut target, mode, &s.types, breakdown, s.tree_shown());
            let counters = debug::Counters::of(engine);
            let extras = extras::read(m, s, engine.budget(), counters);
            (snap, extras, t.elapsed())
        });
        if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
            return Err(io::Error::other(msg));
        }
        let p = &mut self.pacing;
        (p.last_read, p.snap_cost, p.read_cost, p.read_mode) = (Some(now), cost, cost, mode);
        p.seen_version = Some(snap.version);
        if snap.focus != p.focus {
            if let (Some((job, dir)), Some(scanner)) = (snap.focus, engine.local_scanner()) {
                scanner.set_focus(job, dir);
            }
            p.focus = snap.focus;
        }
        self.front.state_mut().target = target;
        self.front.refresh(snap, now);
        let s = self.front.state_mut();
        s.outside.request(&s.snap);
        apply(&mut self.front, extras);
        // The refresh moved the selection (another directory, a rescan): read the extras
        // again for the entry this frame shows.
        if extras::stale(self.front.state()) {
            let s = self.front.state();
            let extras = engine.read(|m| {
                let counters = debug::Counters::of(engine);
                extras::read(m, s, engine.budget(), counters)
            });
            apply(&mut self.front, extras);
        }
        self.pacing.data_dirty = false;
        Ok(())
    }

    /// Reads only the extras, for the snapshot already held.
    fn read_extras(&mut self, now: Instant) {
        let engine = self.engine.get();
        let s = self.front.state();
        let (extras, cost) = engine.read(|m| {
            let t = Instant::now();
            let counters = debug::Counters::of(engine);
            let extras = extras::read(m, s, engine.budget(), counters);
            (extras, t.elapsed())
        });
        self.pacing.read_cost = cost;
        apply(&mut self.front, extras);
        self.front.reorder(now);
    }
}

/// Stores what a read found and lets the features and the front end take it in.
fn apply<F: FrontEnd>(front: &mut F, extras: extras::Extras) {
    let applied = extras::apply(front.state_mut(), extras);
    front.applied(applied);
}
