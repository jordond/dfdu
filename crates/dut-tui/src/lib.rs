//! The interactive browser (`docs/research/tui-design.md`). The run loop reads the engine model
//! under the read lock only to copy a [`snapshot::Snapshot`], then draws from the copy.
//!
//! Redraws: at once on input, at most 10 times a second when the engine reports changes, every
//! 100 ms while a spinner is visible, at the automatic re-sort deadline, and never when idle.

mod app;
mod order;
mod snapshot;
mod theme;
mod view;

use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{select, unbounded};
use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{DirId, JobId, JobOptions, PortableBackend};
use ratatui::crossterm::event::{self, Event};

pub use app::{App, Effect, Overlay};
pub use snapshot::{Snapshot, Target};
pub use theme::Theme;

/// Fastest redraw rate for scan data (ncdu's default refresh is 10 per second).
const DATA_FRAME: Duration = Duration::from_millis(100);
/// Longest sleep when nothing is going on.
const IDLE: Duration = Duration::from_secs(3600);

pub struct RunOptions {
    pub path: PathBuf,
    pub cross_mounts: bool,
    pub engine: EngineOptions,
}

/// Opens the browser on `opts.path` and returns when the user quits.
pub fn run(opts: RunOptions) -> io::Result<()> {
    let engine = Engine::start(PortableBackend, opts.engine);
    engine.send(Command::Scan {
        path: opts.path,
        opts: JobOptions {
            cross_mounts: opts.cross_mounts,
        },
    });
    // Sets a panic hook that restores the terminal before the panic message is printed.
    let mut terminal = ratatui::try_init()?;
    // try_init installed a hook that restores the terminal on panic. Panic hooks also run for
    // panics that are caught, and worker and applier panics are caught and recorded by the
    // engine, so for those threads the terminal must be left alone.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let caught_by_engine = matches!(
            std::thread::current().name(),
            Some("dut-worker" | "dut-applier")
        );
        if !caught_by_engine {
            restore(info);
        }
    }));
    let result = event_loop(&mut terminal, &engine);
    ratatui::restore();
    engine.shutdown(Duration::from_millis(200));
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    engine: &Engine<PortableBackend>,
) -> io::Result<()> {
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

    let start = Instant::now();
    let mut app = App::new(Theme::from_env(), start);
    let mut focus = None;
    let mut seen_version = None;
    let mut last_draw: Option<Instant> = None;
    let (mut input_dirty, mut data_dirty) = (true, true);
    loop {
        let now = Instant::now();
        let data_ready =
            data_dirty && last_draw.is_none_or(|t| now.duration_since(t) >= DATA_FRAME);
        let sort_due = app.next_sort_at().is_some_and(|t| now >= t);
        let animating =
            spinners_visible(&app) && last_draw.is_none_or(|t| now.duration_since(t) >= DATA_FRAME);
        if input_dirty || data_ready || sort_due || animating {
            let (snap, target) = {
                let mut target = app.target.clone();
                let apparent = app.apparent;
                let snap = engine.read(|m| snapshot::build(m, &mut target, apparent));
                (snap, target)
            };
            if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
                // The engine stopped; showing stale data as if it were live would mislead.
                return Err(io::Error::other(msg));
            }
            seen_version = Some(snap.version);
            if snap.focus != focus {
                if let Some((job, dir)) = snap.focus {
                    engine.scanner().set_focus(JobId(job), DirId(dir));
                }
                focus = snap.focus;
            }
            app.target = target;
            app.refresh(snap, now);
            terminal.draw(|f| view::draw(f, &mut app, now))?;
            last_draw = Some(now);
            input_dirty = false;
            data_dirty = false;
        }
        if app.quit {
            return Ok(());
        }

        let mut wake = IDLE;
        let since = last_draw.map_or(Duration::ZERO, |t| now.duration_since(t));
        if data_dirty || spinners_visible(&app) {
            wake = wake.min(DATA_FRAME.saturating_sub(since));
        }
        if let Some(t) = app.next_sort_at() {
            wake = wake.min(t.saturating_duration_since(now));
        }
        select! {
            recv(input) -> ev => match ev {
                Ok(Event::Key(k)) => {
                    for fx in app.key(k, Instant::now()) {
                        match fx {
                            Effect::Send(cmd) => engine.send(cmd),
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
mod tests;
