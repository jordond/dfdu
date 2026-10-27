//! The interactive browser. The run loop reads the engine model under the read lock only to copy
//! a [`snapshot::Snapshot`] and the features' `extras::Extras`, then draws from the copy.
//!
//! State and key handling are in `app` (with the action registry in `app::keys`), drawing in
//! `view`. Each feature has its own module with its state, keys and drawing: the overlays
//! `help`, `error_list`, `confirm` and `palette`, and `marks`, `filter`, `details`, `debug`,
//! `motion` and `mouse`.
//!
//! Redraws: at once on input, at most 10 times a second when the engine reports changes (at
//! once until a frame with entries has been drawn), every 100 ms while a spinner is visible, at
//! the automatic re-sort deadline, while an animation runs, and never when idle. Input is drawn
//! from the snapshot held unless it changed the directory or size mode; the model is read again
//! when scan data is due, at most 10 times a second and less often when reading takes long
//! (large directories). When only the extras are out of date (marks changed, another entry
//! selected for the details pane), they are read without copying the directory again.

mod app;
mod confirm;
mod debug;
mod details;
mod error_list;
mod extras;
mod filter;
mod fuzzy;
mod help;
mod input;
mod marks;
mod motion;
mod mouse;
mod order;
mod palette;
mod picker;
mod problems;
mod setup;
mod snapshot;
mod theme;
mod view;

use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, select, unbounded};
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{DirId, JobId, JobOptions};
use ratatui::Terminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;

pub use app::{App, Effect, Overlay};
pub use debug::summary as debug_summary;
pub use picker::{DISK_TIMEOUT, Picked, Picker};
pub use problems::{ErrorItem, ErrorList, errors, errors_below, problem};
pub use setup::{ThemeChoice, UiSettings, theme_dir};
pub use snapshot::{SizeMode, Snapshot, Target, subvol_label};
pub use theme::{ColorLevel, Theme, ThemeError, ThemeFile, human, parse_color_flag};

use extras::Read;

/// Fastest redraw rate for scan data (ncdu's default refresh is 10 per second).
const DATA_FRAME: Duration = Duration::from_millis(100);
/// Most entries the error list shows.
const ERROR_LIST_MAX: usize = 10_000;
/// Longest sleep when nothing is going on.
const IDLE: Duration = Duration::from_secs(3600);
/// How long the terminal gets to answer the background-colour query at start.
const BACKGROUND_QUERY: Duration = Duration::from_secs(1);

pub struct RunOptions {
    /// Directory to browse; `None` shows the disk picker first.
    pub path: Option<PathBuf>,
    /// Options of the scan (which mounts it descends into).
    pub job: JobOptions,
    pub engine: EngineOptions,
    pub ui: UiSettings,
}

/// How the browser looks and behaves, decided before the loop starts.
#[derive(Clone, Debug)]
pub struct UiOptions {
    pub theme: Theme,
    /// The terminal's colour level, which a theme chosen in the palette is resolved for.
    pub color_level: ColorLevel,
    /// The terminal's background colour, when it was queried.
    pub background: Option<theme::Rgb>,
    /// Mouse capture is on, so mouse events are handled.
    pub mouse: bool,
    /// Start with the debug panel open.
    pub debug: bool,
    /// The user's home directory, shown as `~`.
    pub home: Option<PathBuf>,
}

impl UiOptions {
    /// The theme from the environment (`NO_COLOR`) at its own colour level, no mouse, no debug
    /// panel, no `~`.
    pub fn from_env() -> Self {
        let theme = Theme::from_env();
        UiOptions {
            color_level: theme.level(),
            background: None,
            theme,
            mouse: false,
            debug: false,
            home: None,
        }
    }
}

/// Opens the browser on `opts.path` (or a disk chosen in the picker) and returns when the user
/// quits. Before the alternate screen is entered, the colour level is detected and, when the
/// theme depends on it, the terminal is asked for its background colour (waiting at most
/// `BACKGROUND_QUERY` for terminals that do not answer).
pub fn run(opts: RunOptions) -> io::Result<()> {
    let ui = opts.ui;
    let colors = setup::resolve_theme(
        &ui,
        |k| std::env::var_os(k),
        || theme::query_background(BACKGROUND_QUERY),
    );
    let theme = colors.theme;
    if colors.force_color {
        // crossterm leaves out colours itself while `NO_COLOR` is set.
        ratatui::crossterm::style::force_color_output(true);
    }
    let mut terminal = ratatui::try_init()?;
    let mouse = ui.mouse;
    // try_init installed a hook that restores the terminal on panic. Panic hooks also run for
    // panics that are caught, and worker and applier panics are caught and recorded by the
    // engine, so for those threads the terminal must be left alone.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !dfdu_engine::panic_is_caught_by_engine(std::thread::current().name()) {
            // ratatui's hook leaves raw mode and the alternate screen but does not show the
            // cursor or end mouse capture; the terminal's Drop would show the cursor, but only
            // if unwinding gets that far.
            let _ = execute!(io::stdout(), ratatui::crossterm::cursor::Show);
            if mouse {
                let _ = execute!(io::stdout(), DisableMouseCapture);
            }
            restore(info);
        }
    }));
    if mouse && let Err(e) = execute!(io::stdout(), EnableMouseCapture) {
        setup::restore_terminal(mouse);
        return Err(e);
    }
    let path = match opts.path {
        Some(p) => p,
        None => match picker::pick(&mut terminal, &theme) {
            Ok(picker::Picked::Scan(p)) => p,
            Ok(picker::Picked::Quit) => {
                setup::restore_terminal(mouse);
                return Ok(());
            }
            Err(e) => {
                setup::restore_terminal(mouse);
                return Err(e);
            }
        },
    };
    let storage = dfdu_sys::storage(&path);
    let engine_opts = EngineOptions {
        workers: dfdu_engine::workers_for(storage),
        ..opts.engine
    };
    let engine = Engine::start(dfdu_scan::native_backend(storage), engine_opts);
    engine.send(Command::Scan {
        path,
        opts: opts.job,
    });
    let (tx, input) = unbounded();
    let spawned = thread::Builder::new()
        .name("dfdu-input".into())
        .spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        });
    let result = match spawned {
        Ok(_) => event_loop_with(
            &mut terminal,
            &engine,
            input,
            UiOptions {
                theme,
                color_level: colors.level,
                background: colors.background,
                mouse,
                debug: ui.debug_stats,
                home: setup::home_dir(),
            },
        ),
        Err(e) => Err(e),
    };
    setup::restore_terminal(mouse);
    if ui.debug_stats {
        let summary = debug_summary(&engine);
        if !summary.is_empty() {
            eprintln!("{}", summary.trim_end());
        }
    }
    engine.shutdown(Duration::from_millis(200));
    result
}

/// The browser's loop with [`UiOptions::from_env`]. Public for `tests/latency.rs`, which passes
/// a recording backend and a channel it writes keys to.
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
    // Hidden, for `crates/dfdu/tests/ui_panic.rs`: panic on the first key press, to test that a
    // panic on the UI thread restores the terminal and writes a crash report.
    let inject_panic = std::env::var_os("DFDU_TEST_UI_PANIC").is_some();
    let start = Instant::now();
    let mut app = App::new(ui.theme, start);
    app.color_level = ui.color_level;
    app.background = ui.background;
    app.home = ui.home;
    motion::start(&mut app);
    if ui.debug {
        debug::toggle(&mut app);
    }
    let mut focus = None;
    let mut seen_version = None;
    let mut last_draw: Option<Instant> = None;
    // When the snapshot was last read, how long the read held the lock, and its size mode.
    let mut last_read: Option<Instant> = None;
    let mut snap_cost = Duration::ZERO;
    let mut read_mode = app.size_mode;
    // How long the last read of either kind held the lock, for the debug panel.
    let mut read_cost = Duration::ZERO;
    let (mut input_dirty, mut data_dirty) = (true, true);
    // The first frame is drawn before the scan has read anything. Until a frame has shown
    // entries, data is drawn as soon as it arrives instead of waiting for `DATA_FRAME`.
    let mut shown_entries = false;
    loop {
        let now = Instant::now();
        // A read copies the current directory under the read lock, about 100 ms for one of a
        // million entries. Scan data is read at most every `DATA_FRAME`, and at most every
        // nine times the cost of the last read, so the lock is held at most a tenth of the
        // time and the scan keeps going.
        let data_interval = DATA_FRAME.max(snap_cost * 9);
        let data_ready = data_dirty
            && (!shown_entries || last_read.is_none_or(|t| now.duration_since(t) >= data_interval));
        // Keys are drawn from the snapshot already held, except when they opened another
        // directory or changed the size mode.
        let stale = app.target.path != app.snap.dir_path || app.size_mode != read_mode;
        let read = extras::plan(data_ready || stale, app.extras_stale());
        let sort_due = app.next_sort_at().is_some_and(|t| now >= t);
        let animating =
            spinners_visible(&app) && last_draw.is_none_or(|t| now.duration_since(t) >= DATA_FRAME);
        let motion_due = app.motion.next_frame_at().is_some_and(|t| now >= t);
        if input_dirty || read != Read::Nothing || sort_due || animating || motion_due {
            match read {
                Read::Full => {
                    let mut target = app.target.clone();
                    let mode = app.size_mode;
                    let (snap, extras, cost) = engine.read(|m| {
                        let t = Instant::now();
                        let snap = snapshot::build(m, &mut target, mode);
                        let counters = debug::Counters::of(engine);
                        let extras = extras::read(m, &app, engine.budget(), counters);
                        (snap, extras, t.elapsed())
                    });
                    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
                        // The engine stopped; showing stale data as if it were live would
                        // mislead.
                        return Err(io::Error::other(msg));
                    }
                    (last_read, snap_cost, read_cost, read_mode) = (Some(now), cost, cost, mode);
                    seen_version = Some(snap.version);
                    if snap.focus != focus {
                        if let Some((job, dir)) = snap.focus {
                            engine.scanner().set_focus(JobId(job), DirId(dir));
                        }
                        focus = snap.focus;
                    }
                    app.target = target;
                    app.refresh(snap, now);
                    extras::apply(&mut app, extras);
                    // The refresh moved the selection (another directory, a rescan): read the
                    // extras again for the entry this frame draws.
                    if app.extras_stale() {
                        let extras = engine.read(|m| {
                            let counters = debug::Counters::of(engine);
                            extras::read(m, &app, engine.budget(), counters)
                        });
                        extras::apply(&mut app, extras);
                    }
                    data_dirty = false;
                }
                Read::Extras => {
                    let (extras, cost) = engine.read(|m| {
                        let t = Instant::now();
                        let counters = debug::Counters::of(engine);
                        let extras = extras::read(m, &app, engine.budget(), counters);
                        (extras, t.elapsed())
                    });
                    read_cost = cost;
                    extras::apply(&mut app, extras);
                    app.reorder(now);
                }
                Read::Nothing => app.reorder(now),
            }
            let drawing = Instant::now();
            terminal.draw(|f| view::draw(f, &mut app, now))?;
            debug::frame_drawn(&mut app, drawing.elapsed(), read_cost);
            last_draw = Some(now);
            shown_entries |= !app.snap.entries.is_empty();
            input_dirty = false;
        }
        if app.quit {
            return Ok(());
        }

        let mut wake = IDLE;
        let since = |t: Option<Instant>| t.map_or(Duration::ZERO, |t| now.duration_since(t));
        if data_dirty {
            wake = wake.min(
                DATA_FRAME
                    .max(snap_cost * 9)
                    .saturating_sub(since(last_read)),
            );
        }
        if spinners_visible(&app) {
            wake = wake.min(DATA_FRAME.saturating_sub(since(last_draw)));
        }
        if let Some(t) = app.next_sort_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        if let Some(t) = app.motion.next_frame_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        if app.extras_stale() {
            // Something after the last read changed what the extras are read for (such as a
            // re-sort of the snapshot held that moved the selection): read them again at once.
            wake = Duration::ZERO;
        }
        select! {
            recv(input) -> ev => match ev {
                Ok(Event::Key(k)) => {
                    if inject_panic {
                        panic!("injected UI panic");
                    }
                    let fx = app.key(k, Instant::now());
                    handle_effects(&mut app, engine, fx);
                    input_dirty = true;
                }
                Ok(Event::Mouse(m)) if ui.mouse => {
                    if let Some(fx) = mouse::event(&mut app, m, Instant::now()) {
                        handle_effects(&mut app, engine, fx);
                        input_dirty = true;
                    }
                }
                Ok(Event::Resize(..)) => input_dirty = true,
                Ok(_) => {}
                // The input thread ended (terminal closed): leave.
                Err(_) => return Ok(()),
            },
            recv(engine.wake()) -> _ => {
                // The applier wakes only when its fingerprint changed; it can still match what
                // was last drawn (the frame read it already), so compare before redrawing.
                let v = engine.read(|m| m.fingerprint());
                if Some(v) != seen_version {
                    data_dirty = true;
                }
            }
            default(wake) => {}
        }
    }
}

/// Carries out the effects of a key or mouse event.
fn handle_effects<B: dfdu_scan::Backend>(app: &mut App, engine: &Engine<B>, fx: Vec<Effect>) {
    for fx in fx {
        match fx {
            Effect::Send(cmd) => engine.send(cmd),
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
    app.snap.busy
}

#[cfg(test)]
mod blocked;
#[cfg(all(test, target_os = "linux"))]
mod btrfs_tests;
#[cfg(test)]
mod soak;
#[cfg(test)]
mod tests;
