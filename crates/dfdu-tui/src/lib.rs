//! The interactive browser. The run loop (`run_loop`) ticks the session driver
//! (`dfdu_session::Session`), which reads the engine model under the read lock only to copy a
//! `model::snapshot::Snapshot` and the features' `model::extras::Extras`; the loop draws from
//! the copy.
//!
//! State and key handling are in `app` (with the action registry in `app::keys`), drawing in
//! `view`, with the styles in `theme` and the text for sizes, counts and bars in `format`. Each
//! feature has its own module with its state, keys and drawing: the views and panels beside the
//! list in `features`, and the overlays drawn over it in `overlay`. The state of the list's rows
//! is in `state`, the text input, the mouse and fuzzy matching in `input`, and what is read from
//! outside the UI (the engine model, the scan history and cache, the mounts outside the scan) in
//! `model`; only the duplicates comparison and the disk picker read files and disks themselves.
//! Each of these lists its modules.
//!
//! Redraws: at once on input, at most 10 times a second when the engine reports changes (at
//! once until a frame with entries has been drawn), every 100 ms while a spinner is visible, at
//! the automatic re-sort deadline, while an animation runs, and never when idle. Input is drawn
//! from the snapshot held unless it changed the directory or size mode; the model is read again
//! when scan data is due, at most 10 times a second and less often when reading takes long
//! (large directories). When only the extras are out of date (marks changed, another entry
//! selected for the details pane), they are read without copying the directory again.

mod app;
mod features;
mod format;
mod input;
mod model;
mod overlay;
mod run_loop;
mod setup;
mod state;
mod theme;
mod view;

use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crossbeam_channel::unbounded;
use dfdu_engine::{Engine, EngineOptions};
use dfdu_session::startup::{self, Opened, Setup};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;

use features::picker::Picked;

pub use app::{IDLE_BEFORE_SORT, MIN_SORT_INTERVAL};
pub use dfdu_session::cache_link::CacheLink;
pub use dfdu_session::history_link::HistoryLink;
pub use dfdu_session::startup::{Source, StartLocal};
pub use features::debug::summary as debug_summary;
pub use run_loop::{event_loop, event_loop_with};
pub use setup::{ThemeChoice, UiSettings};
pub use state::motion::MAX_STEP as MAX_EASING_STEP;
pub use theme::{Bars, BuiltinTheme, ColorLevel, Theme, ThemeError, ThemeFile, parse_color_flag};

/// How long the terminal gets to answer the background-colour query at start.
const BACKGROUND_QUERY: Duration = Duration::from_secs(1);

pub struct RunOptions {
    pub source: Source,
    /// Options of engines the browser starts (all but [`Source::Remote`]'s). Its `read_only`
    /// also tells the browser to leave out the delete keys, for a remote engine as well.
    pub engine: EngineOptions,
    pub ui: UiSettings,
    /// Record a scan of this host's files in the history once it finishes, and show what grew
    /// since the last.
    pub history: bool,
}

/// How the browser looks and behaves, decided before the loop starts.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent options of the browser, not one state spread over several bools"
)]
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
    /// The user's home directory, shown as `~` in paths of a scan of this machine.
    pub home: Option<PathBuf>,
    /// The scan history this browser reads and adds to.
    pub history: Option<HistoryLink>,
    /// The scan cache: what the browser opens (it sends the first scan or resume itself) and
    /// saves complete scans to.
    pub cache: Option<CacheLink>,
    /// The file type filter to start with.
    pub types: dfdu_tree::filetype::TypeFilter,
    /// The scan is of this machine's disks: mounts it did not enter and links out of it are
    /// looked up here (not for imported or remote scans).
    pub local: bool,
    /// Nothing is deleted (`--read-only`, imported scans): the delete keys are left out and
    /// cleanup commands run only as dry runs.
    pub read_only: bool,
}

impl UiOptions {
    /// The theme [`run`] would choose from the environment with the default settings, without
    /// asking the terminal for its background; no mouse, no debug panel, no `~`.
    #[must_use]
    pub fn from_env() -> Self {
        let colors = setup::resolve_theme(&UiSettings::default(), |k| std::env::var_os(k), || None);
        UiOptions {
            theme: colors.theme,
            color_level: colors.level,
            background: colors.background,
            mouse: false,
            debug: false,
            home: None,
            history: None,
            cache: None,
            types: dfdu_tree::filetype::TypeFilter::default(),
            local: false,
            read_only: false,
        }
    }
}

/// Opens the browser on `opts.source` (or a disk chosen in the picker) and returns when the user
/// quits. Before the alternate screen is entered, the colour level is detected and, when the
/// theme depends on it, the terminal is asked for its background colour (waiting at most
/// `BACKGROUND_QUERY` for terminals that do not answer).
///
/// # Errors
///
/// When the terminal cannot be set up or drawn to, the disk picker fails, a resumed scan has
/// no root, or the engine stops with a fatal error.
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
    let restore = setup::Restore { mouse };
    restore_on_panic(mouse);
    if mouse {
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    let source = match opts.source {
        Source::Local { path: None, start } => match features::picker::pick(&mut terminal, &theme)?
        {
            Picked::Scan(p) => Source::Local {
                path: Some(p),
                start,
            },
            Picked::Quit => return Ok(()),
        },
        source => source,
    };
    let ui_opts = |setup: Setup| UiOptions {
        theme,
        color_level: colors.level,
        background: colors.background,
        mouse,
        debug: ui.debug_stats,
        home: setup.home,
        history: setup.jobs.history,
        cache: setup.jobs.cache,
        types: ui.types.clone(),
        local: setup.local,
        read_only: setup.read_only,
    };
    match startup::open(source, &opts.engine, opts.history)? {
        Opened::Native(s) => browse(&mut terminal, restore, s.engine, ui_opts(s.setup)),
        Opened::Imported(s) => browse(&mut terminal, restore, s.engine, ui_opts(s.setup)),
    }
}

/// Restores the terminal on a panic of the UI thread. `ratatui::try_init` installed a hook that
/// restores it on any panic; panic hooks also run for panics that are caught, and worker and
/// applier panics are caught and recorded by the engine, so for those threads the terminal
/// must be left alone.
fn restore_on_panic(mouse: bool) {
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
}

/// Runs the loop on `engine` until the user quits, then restores the terminal (`restore`) and
/// stops the engine.
fn browse<B: dfdu_scan::Backend>(
    terminal: &mut ratatui::DefaultTerminal,
    restore: setup::Restore,
    engine: Engine<B>,
    ui: UiOptions,
) -> io::Result<()> {
    let debug_stats = ui.debug;
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
        Ok(_) => event_loop_with(terminal, &engine, input, ui),
        Err(e) => Err(e),
    };
    drop(restore);
    if debug_stats {
        let summary = debug_summary(&engine);
        if !summary.is_empty() {
            eprintln!("{}", summary.trim_end());
        }
    }
    engine.shutdown(Duration::from_millis(200));
    result
}

#[cfg(test)]
mod tests;
