//! The browser's loop: reads the engine model when scan data or input calls for it, draws, and
//! sleeps until the next key, engine change or timed wake. The pacing rules are in the crate
//! docs.

use std::io;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, select};
use dfdu_engine::{Engine, ErrorList, SizeMode, errors};
use dfdu_scan::{DirId, JobId};
use ratatui::Terminal;
use ratatui::crossterm::event::Event;

use crate::app::{self, App, Effect};
use crate::features::{debug, dupes};
use crate::input::mouse;
use crate::model::extras::{self, Read};
use crate::model::history_link::Recorder;
use crate::model::{outside, snapshot};
use crate::state::motion;
use crate::{UiOptions, overlay, view};

/// Fastest redraw rate for scan data (ncdu's default refresh is 10 per second).
const DATA_FRAME: Duration = Duration::from_millis(100);
/// Most entries the error list shows.
const ERROR_LIST_MAX: usize = 10_000;
/// Longest sleep when nothing is going on.
const IDLE: Duration = Duration::from_secs(3600);

/// The browser's loop with [`UiOptions::from_env`]. Public for `tests/latency.rs`, which passes
/// a recording backend and a channel it writes keys to.
///
/// # Errors
///
/// As [`event_loop_with`].
pub fn event_loop<T, B>(
    terminal: &mut Terminal<T>,
    engine: &Engine<B>,
    input: Receiver<Event>,
) -> io::Result<()>
where
    T: ratatui::backend::Backend<Error = io::Error>,
    B: dfdu_scan::Backend,
{
    event_loop_with(terminal, engine, input, UiOptions::from_env())
}

/// The browser's loop: terminal events from `input`, frames to `terminal`. Returns when the user
/// quits or `input` closes.
///
/// # Errors
///
/// When drawing to `terminal` fails, or the engine stopped with a fatal error (showing its
/// stale data as if it were live would mislead).
///
/// # Panics
///
/// On the first key press when `DFDU_TEST_UI_PANIC` is set: a hidden switch for
/// `crates/dfdu/tests/ui_panic.rs`, which tests that a panic on the UI thread restores the
/// terminal and writes a crash report.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the loop owns the input channel, so the input thread's sends fail once it returns"
)]
pub fn event_loop_with<T, B>(
    terminal: &mut Terminal<T>,
    engine: &Engine<B>,
    input: Receiver<Event>,
    ui: UiOptions,
) -> io::Result<()>
where
    T: ratatui::backend::Backend<Error = io::Error>,
    B: dfdu_scan::Backend,
{
    let inject_panic = std::env::var_os("DFDU_TEST_UI_PANIC").is_some();
    let UiOptions {
        theme,
        color_level,
        background,
        mouse: mouse_on,
        debug: debug_on,
        home,
        history,
        types,
        local,
        read_only,
    } = ui;
    let start = Instant::now();
    let mut app = App::new(theme, start);
    app.color_level = color_level;
    app.background = background;
    app.home = home;
    app.types = types;
    app.outside.local = local;
    app.read_only = read_only;
    motion::start(&mut app);
    if debug_on {
        debug::toggle(&mut app);
    }
    let mut recorder = Recorder::new(history);
    let mut pacing = Pacing::new(app.size_mode);
    loop {
        let history_changed = recorder.poll(&mut app, engine);
        let outside_changed = app.outside.poll();
        if history_changed || outside_changed {
            pacing.input_dirty = true;
        }
        let now = Instant::now();
        if let Some(read) = pacing.frame_due(&app, now) {
            match read {
                Read::Full => pacing.read_full(&mut app, engine, now)?,
                Read::Extras => pacing.read_extras(&mut app, engine, now),
                Read::Nothing => app.reorder(now),
            }
            let drawing = Instant::now();
            terminal.draw(|f| view::draw(f, &mut app, now))?;
            debug::frame_drawn(&mut app, drawing.elapsed(), pacing.read_cost);
            pacing.drawn(&app, now);
        }
        if app.quit {
            return Ok(());
        }

        let wake = pacing.wake(&app, &recorder, now);
        select! {
            recv(input) -> ev => match ev {
                Ok(Event::Key(k)) => {
                    assert!(!inject_panic, "injected UI panic");
                    let fx = app.key(k, Instant::now());
                    handle_effects(&mut app, engine, fx);
                    pacing.input_dirty = true;
                }
                Ok(Event::Mouse(m)) if mouse_on => {
                    if let Some(fx) = mouse::event(&mut app, m, Instant::now()) {
                        handle_effects(&mut app, engine, fx);
                        pacing.input_dirty = true;
                    }
                }
                Ok(Event::Resize(..)) => pacing.input_dirty = true,
                Ok(_) => {}
                // The input thread ended (terminal closed): leave.
                Err(_) => return Ok(()),
            },
            recv(engine.changes()) -> _ => {
                // The applier wakes only when its fingerprint changed; it can still match what
                // was last drawn (the frame read it already), so compare before redrawing.
                let v = engine.read(dfdu_engine::Model::fingerprint);
                if Some(v) != pacing.seen_version {
                    pacing.data_dirty = true;
                }
            }
            default(wake) => {}
        }
    }
}

/// What the loop remembers between frames to decide when to read the model and draw.
struct Pacing {
    /// The directory the scanner was last asked to scan first.
    focus: Option<(JobId, DirId)>,
    /// The model's fingerprint at the last full read.
    seen_version: Option<u64>,
    last_draw: Option<Instant>,
    /// When the snapshot was last read, how long the read held the lock, and its size mode.
    last_read: Option<Instant>,
    snap_cost: Duration,
    read_mode: SizeMode,
    /// How long the last read of either kind held the lock, for the debug panel.
    read_cost: Duration,
    input_dirty: bool,
    data_dirty: bool,
    /// The first frame is drawn before the scan has read anything. Until a frame has shown
    /// entries, data is drawn as soon as it arrives instead of waiting for `DATA_FRAME`.
    shown_entries: bool,
}

impl Pacing {
    fn new(read_mode: SizeMode) -> Self {
        Pacing {
            focus: None,
            seen_version: None,
            last_draw: None,
            last_read: None,
            snap_cost: Duration::ZERO,
            read_mode,
            read_cost: Duration::ZERO,
            input_dirty: true,
            data_dirty: true,
            shown_entries: false,
        }
    }

    /// Scan data is read at most every `DATA_FRAME`, and at most every nine times the cost of
    /// the last read: a read copies the current directory under the read lock, about 100 ms for
    /// one of a million entries, so the lock is held at most a tenth of the time and the scan
    /// keeps going.
    fn data_interval(&self) -> Duration {
        DATA_FRAME.max(self.snap_cost * 9)
    }

    /// What to read before drawing a frame now, or `None` when no frame is due.
    fn frame_due(&self, app: &App, now: Instant) -> Option<Read> {
        let data_ready = self.data_dirty
            && (!self.shown_entries
                || self
                    .last_read
                    .is_none_or(|t| now.duration_since(t) >= self.data_interval()));
        // Keys are drawn from the snapshot already held, except when they opened another
        // directory or changed the size mode.
        let stale = app.target.path != app.snap.dir_path
            || app.size_mode != self.read_mode
            || overlay::type_filter::stale(app);
        let read = extras::plan(data_ready || stale, app.extras_stale());
        let sort_due = app.next_sort_at().is_some_and(|t| now >= t);
        let animating = spinners_visible(app)
            && self
                .last_draw
                .is_none_or(|t| now.duration_since(t) >= DATA_FRAME);
        let motion_due = app.motion.next_frame_at().is_some_and(|t| now >= t);
        (self.input_dirty || read != Read::Nothing || sort_due || animating || motion_due)
            .then_some(read)
    }

    /// Reads a new snapshot and the extras, and folds them into `app`.
    fn read_full<B: dfdu_scan::Backend>(
        &mut self,
        app: &mut App,
        engine: &Engine<B>,
        now: Instant,
    ) -> io::Result<()> {
        let mut target = app.target.clone();
        let mode = app.size_mode;
        let breakdown = matches!(app.overlay, app::Overlay::Types(_));
        let (snap, extras, cost) = engine.read(|m| {
            let t = Instant::now();
            let snap = snapshot::build_view(m, &mut target, mode, &app.types, breakdown);
            let counters = debug::Counters::of(engine);
            let extras = extras::read(m, app, engine.budget(), counters);
            (snap, extras, t.elapsed())
        });
        if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
            // The engine stopped; showing stale data as if it were live would mislead.
            return Err(io::Error::other(msg));
        }
        (
            self.last_read,
            self.snap_cost,
            self.read_cost,
            self.read_mode,
        ) = (Some(now), cost, cost, mode);
        self.seen_version = Some(snap.version);
        if snap.focus != self.focus {
            if let Some((job, dir)) = snap.focus {
                engine.scanner().set_focus(job, dir);
            }
            self.focus = snap.focus;
        }
        app.target = target;
        app.refresh(snap, now);
        outside::request(app);
        extras::apply(app, extras);
        // The refresh moved the selection (another directory, a rescan): read the extras
        // again for the entry this frame draws.
        if app.extras_stale() {
            let extras = engine.read(|m| {
                let counters = debug::Counters::of(engine);
                extras::read(m, app, engine.budget(), counters)
            });
            extras::apply(app, extras);
        }
        self.data_dirty = false;
        Ok(())
    }

    /// Reads only the extras, for the snapshot already held.
    fn read_extras<B: dfdu_scan::Backend>(
        &mut self,
        app: &mut App,
        engine: &Engine<B>,
        now: Instant,
    ) {
        let (extras, cost) = engine.read(|m| {
            let t = Instant::now();
            let counters = debug::Counters::of(engine);
            let extras = extras::read(m, app, engine.budget(), counters);
            (extras, t.elapsed())
        });
        self.read_cost = cost;
        extras::apply(app, extras);
        app.reorder(now);
    }

    /// Notes a frame drawn at `now`.
    fn drawn(&mut self, app: &App, now: Instant) {
        self.last_draw = Some(now);
        self.shown_entries |= !app.snap.entries.is_empty();
        self.input_dirty = false;
    }

    /// How long to wait for input or engine changes before the next timed frame.
    fn wake(&self, app: &App, recorder: &Recorder, now: Instant) -> Duration {
        let mut wake = IDLE;
        let since = |t: Option<Instant>| t.map_or(Duration::ZERO, |t| now.duration_since(t));
        if self.data_dirty {
            wake = wake.min(self.data_interval().saturating_sub(since(self.last_read)));
        }
        if spinners_visible(app) {
            wake = wake.min(DATA_FRAME.saturating_sub(since(self.last_draw)));
        }
        if let Some(t) = app.next_sort_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        if let Some(t) = app.motion.next_frame_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        if recorder.busy() || app.outside.pending() {
            wake = wake.min(DATA_FRAME);
        }
        if app.extras_stale() {
            // Something after the last read changed what the extras are read for (such as a
            // re-sort of the snapshot held that moved the selection): read them again at once.
            wake = Duration::ZERO;
        }
        wake
    }
}

/// Carries out the effects of a key or mouse event.
fn handle_effects<B: dfdu_scan::Backend>(app: &mut App, engine: &Engine<B>, fx: Vec<Effect>) {
    for fx in fx {
        match fx {
            Effect::Send(cmd) => engine.send(cmd),
            Effect::FindDupes => {
                let (root, cands) = engine.read(|m| match m.tree.roots().next() {
                    Some(r) => (
                        m.tree.path_buf(r),
                        dfdu_dupes::candidates(&m.tree, r, dupes::min_size()),
                    ),
                    None => (std::path::PathBuf::new(), Vec::new()),
                });
                dupes::begin(app, root, cands);
            }
            Effect::ShowErrors => {
                let list = engine.read(|m| match m.tree.roots().next() {
                    Some(root) => errors(&m.tree, root, ERROR_LIST_MAX),
                    None => ErrorList::default(),
                });
                app.show_errors(list);
            }
        }
    }
}

/// The header spinner and clock move while the engine is busy; nothing animates otherwise.
fn spinners_visible(app: &App) -> bool {
    app.snap.busy || dupes::running(app)
}
