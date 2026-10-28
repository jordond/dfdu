//! The interactive browser. The run loop (`run_loop`) reads the engine model under the read lock
//! only to copy a [`snapshot::Snapshot`] and the features' `extras::Extras`, then draws from the
//! copy.
//!
//! State and key handling are in `app` (with the action registry in `app::keys`), drawing in
//! `view`. Each feature has its own module with its state, keys and drawing: the views
//! `treemap`, `cleanup` and `dupes`; the overlays in `overlay` (`help`, `error_list`,
//! `confirm`, `palette` and `type_filter`); the disk picker `picker`; and `marks`, `filter`,
//! `details`, `growth`, `debug`, `motion` and `mouse`. `history_link` reads and records the
//! scan history.
//!
//! Redraws: at once on input, at most 10 times a second when the engine reports changes (at
//! once until a frame with entries has been drawn), every 100 ms while a spinner is visible, at
//! the automatic re-sort deadline, while an animation runs, and never when idle. Input is drawn
//! from the snapshot held unless it changed the directory or size mode; the model is read again
//! when scan data is due, at most 10 times a second and less often when reading takes long
//! (large directories). When only the extras are out of date (marks changed, another entry
//! selected for the details pane), they are read without copying the directory again.

mod app;
mod cleanup;
mod debug;
mod details;
mod dupes;
mod extras;
mod filter;
mod fuzzy;
mod growth;
mod history_link;
mod input;
mod marks;
mod motion;
mod mouse;
mod order;
mod overlay;
mod picker;
mod run_loop;
mod setup;
mod snapshot;
mod theme;
mod treemap;
mod view;

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crossbeam_channel::unbounded;
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::JobOptions;
use dfdu_store::history;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;

pub use app::{App, Effect, IDLE_BEFORE_SORT, MIN_SORT_INTERVAL, Overlay};
pub use debug::summary as debug_summary;
pub use history_link::HistoryLink;
pub use picker::{Picked, Picker};
pub use run_loop::{event_loop, event_loop_with};
pub use setup::{ThemeChoice, UiSettings, theme_dir};
pub use snapshot::{Snapshot, Target};
pub use theme::{ColorLevel, Theme, ThemeError, ThemeFile, parse_color_flag};

/// How long the terminal gets to answer the background-colour query at start.
const BACKGROUND_QUERY: Duration = Duration::from_secs(1);

/// Starts an engine scanning a directory on this host with the options given, and returns it
/// with the options of the scan (which mounts it descends into, the rules it classifies with).
/// Nothing is sent to it yet.
pub type StartLocal =
    Box<dyn FnOnce(&Path, EngineOptions) -> (Engine<dfdu_scan::NativeBackend>, JobOptions)>;

/// What the browser opens.
pub enum Source {
    /// A directory on this host, scanned by the engine `start` returns; `None` shows the disk
    /// picker first.
    Local {
        path: Option<PathBuf>,
        start: StartLocal,
    },
    /// A scan read from a file, shown at once while its root is rescanned by the engine `start`
    /// returns. When `journal`, where the change journal stood when the scan was taken, can
    /// tell what changed since, only that is rescanned.
    Resumed {
        tree: dfdu_tree::Tree,
        journal: Option<dfdu_store::snapshot::Journal>,
        start: StartLocal,
    },
    /// A scan read from a file, browsed read-only.
    Imported(dfdu_tree::Tree),
    /// An engine scanning `path` on another host with `job`: read-only, nothing is recorded in
    /// the history.
    Remote {
        engine: Engine<dfdu_scan::NativeBackend>,
        path: PathBuf,
        job: JobOptions,
    },
}

pub struct RunOptions {
    pub source: Source,
    /// Options of engines the browser starts (all but [`Source::Remote`]'s).
    pub engine: EngineOptions,
    pub ui: UiSettings,
    /// Record a scan of this host's files in the history once it finishes, and show what grew
    /// since the last.
    pub history: bool,
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
    /// The scan history this browser reads and adds to.
    pub history: Option<HistoryLink>,
    /// The file type filter to start with.
    pub types: dfdu_tree::filetype::TypeFilter,
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
            types: dfdu_tree::filetype::TypeFilter::default(),
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
    restore_on_panic(mouse);
    if mouse && let Err(e) = execute!(io::stdout(), EnableMouseCapture) {
        setup::restore_terminal(mouse);
        return Err(e);
    }
    let ui_opts = |theme: Theme, history: Option<HistoryLink>| UiOptions {
        theme,
        color_level: colors.level,
        background: colors.background,
        mouse,
        debug: ui.debug_stats,
        home: setup::home_dir(),
        history,
        types: ui.types.clone(),
    };
    // Only scans that count the same data are compared.
    let link = |root: &Path, job: &JobOptions, engine: &EngineOptions| {
        let db = history::default_path().filter(|_| opts.history)?;
        let key = history::options_key(job.cross_mounts, job.network, engine.measure_workers > 0);
        Some(HistoryLink::start(db, root.to_path_buf(), key))
    };
    let (path, start) = match opts.source {
        Source::Imported(tree) => {
            let engine = Engine::start(
                dfdu_scan::PortableBackend::default(),
                EngineOptions {
                    imported: true,
                    ..opts.engine
                },
            );
            engine.load(tree);
            return browse(&mut terminal, engine, ui_opts(theme, None));
        }
        Source::Remote { engine, path, job } => {
            engine.send(Command::Scan { path, opts: job });
            return browse(&mut terminal, engine, ui_opts(theme, None));
        }
        Source::Resumed {
            tree,
            journal,
            start,
        } => {
            let Some((engine, path, job)) = resume(tree, journal, start, &opts.engine) else {
                setup::restore_terminal(mouse);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the scan read from the file has no root",
                ));
            };
            let history = link(&path, &job, &opts.engine);
            return browse(&mut terminal, engine, ui_opts(theme, history));
        }
        Source::Local { path, start } => (path, start),
    };
    let path = match path {
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
    let root = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let (engine, job) = start(&path, opts.engine.clone());
    let history = link(&root, &job, &opts.engine);
    engine.send(Command::Scan { path, opts: job });
    browse(&mut terminal, engine, ui_opts(theme, history))
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

/// Starts an engine on the root of `tree`, loads the tree into it and rescans what changed
/// since `journal`, or all of it. Returns the engine with the root's path and the options of
/// the scan; `None` when the tree has no root.
fn resume(
    tree: dfdu_tree::Tree,
    journal: Option<dfdu_store::snapshot::Journal>,
    start: StartLocal,
    engine: &EngineOptions,
) -> Option<(Engine<dfdu_scan::NativeBackend>, PathBuf, JobOptions)> {
    let root = tree.roots().next()?;
    let path = tree.path_buf(root);
    let (engine, job) = start(&path, engine.clone());
    let node = tree.node_ref(root);
    engine.load(tree);
    let changed =
        journal.and_then(|j| dfdu_engine::changed_since(&path, j.volume, j.event_id, j.time));
    engine.send(Command::Resume {
        node,
        opts: job.clone(),
        changed,
    });
    Some((engine, path, job))
}

/// Runs the loop on `engine` until the user quits, then restores the terminal and stops the
/// engine.
fn browse<B: dfdu_scan::Backend>(
    terminal: &mut ratatui::DefaultTerminal,
    engine: Engine<B>,
    ui: UiOptions,
) -> io::Result<()> {
    let (mouse, debug_stats) = (ui.mouse, ui.debug);
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
    setup::restore_terminal(mouse);
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
