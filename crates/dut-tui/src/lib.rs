//! The interactive browser (`docs/research/tui-design.md`). The run loop reads the engine model
//! under the read lock only to copy a [`snapshot::Snapshot`], then draws from the copy.
//!
//! Redraws: at once on input, at most 10 times a second when the engine reports changes (at
//! once until a frame with entries has been drawn), every 100 ms while a spinner is visible, at
//! the automatic re-sort deadline, and never when idle. Input is drawn from the snapshot held
//! unless it changed the directory or size mode; the model is read again when scan data is
//! due, at most 10 times a second and less often when reading takes long (large directories).

mod app;
mod order;
mod picker;
mod problems;
mod snapshot;
mod theme;
mod view;

use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, select, unbounded};
use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{DirId, JobId, JobOptions};
use ratatui::Terminal;
use ratatui::crossterm::event::{self, Event};

pub use app::{App, Effect, Overlay};
pub use picker::{DISK_TIMEOUT, Picked, Picker};
pub use problems::{ErrorItem, ErrorList, errors, errors_below, problem};
pub use snapshot::{SizeMode, Snapshot, Target, subvol_label};
pub use theme::{Theme, human};

/// Fastest redraw rate for scan data (ncdu's default refresh is 10 per second).
const DATA_FRAME: Duration = Duration::from_millis(100);
/// Most entries the error list shows.
const ERROR_LIST_MAX: usize = 10_000;
/// Longest sleep when nothing is going on.
const IDLE: Duration = Duration::from_secs(3600);

pub struct RunOptions {
    /// Directory to browse; `None` shows the disk picker first.
    pub path: Option<PathBuf>,
    /// Options of the scan (which mounts it descends into).
    pub job: JobOptions,
    pub engine: EngineOptions,
}

/// Opens the browser on `opts.path` (or a disk chosen in the picker) and returns when the user
/// quits.
pub fn run(opts: RunOptions) -> io::Result<()> {
    // Sets a panic hook that restores the terminal before the panic message is printed.
    let mut terminal = ratatui::try_init()?;
    // try_init installed a hook that restores the terminal on panic. Panic hooks also run for
    // panics that are caught, and worker and applier panics are caught and recorded by the
    // engine, so for those threads the terminal must be left alone.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !dut_engine::panic_is_caught_by_engine(std::thread::current().name()) {
            // ratatui's hook leaves raw mode and the alternate screen but does not show the
            // cursor; the terminal's Drop would, but only if unwinding gets that far.
            let _ = ratatui::crossterm::execute!(io::stdout(), ratatui::crossterm::cursor::Show);
            restore(info);
        }
    }));
    let path = match opts.path {
        Some(p) => p,
        None => match picker::pick(&mut terminal, &Theme::from_env()) {
            Ok(picker::Picked::Scan(p)) => p,
            Ok(picker::Picked::Quit) => {
                ratatui::restore();
                return Ok(());
            }
            Err(e) => {
                ratatui::restore();
                return Err(e);
            }
        },
    };
    let storage = dut_sys::storage(&path);
    let engine_opts = EngineOptions {
        workers: dut_engine::workers_for(storage),
        ..opts.engine
    };
    let engine = Engine::start(dut_scan::native_backend(storage), engine_opts);
    engine.send(Command::Scan {
        path,
        opts: opts.job,
    });
    let (tx, input) = unbounded();
    thread::Builder::new()
        .name("dut-input".into())
        .spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        })?;
    let result = event_loop(&mut terminal, &engine, input);
    ratatui::restore();
    engine.shutdown(Duration::from_millis(200));
    result
}

/// The browser's loop: terminal events from `input`, frames to `terminal`. Returns when the user
/// quits or `input` closes. Public for `tests/latency.rs`, which passes a recording backend and
/// a channel it writes keys to.
pub fn event_loop<T, B>(
    terminal: &mut Terminal<T>,
    engine: &Engine<B>,
    input: Receiver<Event>,
) -> io::Result<()>
where
    T: ratatui::backend::Backend<Error = io::Error>,
    B: dut_scan::Backend,
{
    // Hidden, for `crates/dut/tests/ui_panic.rs`: panic on the first key press, to test that a
    // panic on the UI thread restores the terminal and writes a crash report.
    let inject_panic = std::env::var_os("DUT_TEST_UI_PANIC").is_some();
    let start = Instant::now();
    let mut app = App::new(Theme::from_env(), start);
    let mut focus = None;
    let mut seen_version = None;
    let mut last_draw: Option<Instant> = None;
    // When the snapshot was last read, how long the read held the lock, and its size mode.
    let mut last_read: Option<Instant> = None;
    let mut read_cost = Duration::ZERO;
    let mut read_mode = app.size_mode;
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
        let data_interval = DATA_FRAME.max(read_cost * 9);
        let data_ready = data_dirty
            && (!shown_entries || last_read.is_none_or(|t| now.duration_since(t) >= data_interval));
        // Keys are drawn from the snapshot already held, except when they opened another
        // directory or changed the size mode.
        let stale = app.target.path != app.snap.dir_path || app.size_mode != read_mode;
        let read = data_ready || stale;
        let sort_due = app.next_sort_at().is_some_and(|t| now >= t);
        let animating =
            spinners_visible(&app) && last_draw.is_none_or(|t| now.duration_since(t) >= DATA_FRAME);
        if input_dirty || read || sort_due || animating {
            if read {
                let mut target = app.target.clone();
                let mode = app.size_mode;
                let (snap, cost) = engine.read(|m| {
                    let t = Instant::now();
                    (snapshot::build(m, &mut target, mode), t.elapsed())
                });
                if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
                    // The engine stopped; showing stale data as if it were live would mislead.
                    return Err(io::Error::other(msg));
                }
                (last_read, read_cost, read_mode) = (Some(now), cost, mode);
                seen_version = Some(snap.version);
                if snap.focus != focus {
                    if let Some((job, dir)) = snap.focus {
                        engine.scanner().set_focus(JobId(job), DirId(dir));
                    }
                    focus = snap.focus;
                }
                app.target = target;
                app.refresh(snap, now);
                data_dirty = false;
            } else {
                app.reorder(now);
            }
            terminal.draw(|f| view::draw(f, &mut app, now))?;
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
                    .max(read_cost * 9)
                    .saturating_sub(since(last_read)),
            );
        }
        if spinners_visible(&app) {
            wake = wake.min(DATA_FRAME.saturating_sub(since(last_draw)));
        }
        if let Some(t) = app.next_sort_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        select! {
            recv(input) -> ev => match ev {
                Ok(Event::Key(k)) => {
                    if inject_panic {
                        panic!("injected UI panic");
                    }
                    for fx in app.key(k, Instant::now()) {
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
                    input_dirty = true;
                }
                Ok(Event::Resize(..)) => input_dirty = true,
                Ok(_) => {}
                // The input thread ended (terminal closed): leave.
                Err(_) => return Ok(()),
            },
            recv(engine.wake()) -> _ => {
                // The applier wakes only when its fingerprint changed; it can still match what
                // was last drawn (the frame read it already), so compare before redrawing.
                let v = engine.read(snapshot::version);
                if Some(v) != seen_version {
                    data_dirty = true;
                }
            }
            default(wake) => {}
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
