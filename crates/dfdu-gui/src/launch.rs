//! What the window opens, built as `dfdu` builds it ([`prepare`]): a scan read from a file
//! (`--import`), a directory on this host opened from its cached scan when there is one, or the
//! start screen; and the engine and session of what is opened ([`open`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_engine::{Engine, EngineOptions};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, NativeBackend, native_backend};
use dfdu_session::startup::{self, Opened, Source, StartLocal};

use crate::args::Args;
use crate::browser::{Browse, Browser};

/// How what the user opens is started.
pub struct Settings {
    pub engine: EngineOptions,
    /// Scans of this machine are recorded in the history.
    pub record: bool,
}

/// What the window opens, and how.
pub struct Launch {
    /// A [`Source::Local`] without a path opens the start screen.
    pub source: Source,
    pub settings: Settings,
}

/// What `args` open, as `dfdu` opens them: an imported scan is browsed read-only and recorded
/// nowhere; a directory named is opened from its cached scan when there is one.
///
/// # Errors
///
/// The scan read from a file could not be read: the message says why.
pub fn prepare(args: &Args) -> Result<Launch, String> {
    let engine = EngineOptions {
        read_only: args.read_only,
        ..EngineOptions::from_env()
    };
    let settings = Settings {
        engine,
        record: args.import.is_none(),
    };
    if let Some(file) = &args.import {
        let tree = import(file).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Launch {
            source: Source::Imported(tree),
            settings,
        });
    }
    let rules = (!args.no_rules).then(load_rules);
    let slot = args.path.as_deref().and_then(cache_slot);
    let start = local(rules);
    let source = match (args.path.clone(), slot) {
        (Some(path), Some(slot)) => Source::Cached { path, slot, start },
        (path, _) => Source::Local { path, start },
    };
    Ok(Launch { source, settings })
}

/// Starts the engine of `source` and the session that browses it, which asks `ctx` for a frame
/// when the engine's model changes.
///
/// # Errors
///
/// As [`startup::open`].
pub fn open(
    source: Source,
    settings: &Settings,
    ctx: &egui::Context,
) -> io::Result<Box<dyn Browse>> {
    Ok(
        match startup::open(source, &settings.engine, settings.record)? {
            Opened::Native(s) => Box::new(Browser::new(s, ctx)),
            Opened::Imported(s) => Box::new(Browser::new(s, ctx)),
        },
    )
}

/// The source the start screen opens for `path`: scanned afresh, as a disk chosen in `dfdu`'s
/// picker is.
pub fn picked(path: PathBuf, start: StartLocal) -> Source {
    Source::Local {
        path: Some(path),
        start,
    }
}

/// Starts engines on this host's files as `dfdu` does: the native backend for the storage the
/// directory is on, with as many workers as suit it, classifying with `rules`. Mounts are not
/// entered, and NTFS volumes are listed directory by directory.
pub fn local(rules: Option<Arc<RuleSet>>) -> StartLocal {
    Box::new(move |path, opts| start(path, opts, rules))
}

fn start(
    path: &Path,
    opts: EngineOptions,
    rules: Option<Arc<RuleSet>>,
) -> (Engine<NativeBackend>, JobOptions) {
    let storage = dfdu_sys::storage(path);
    let opts = EngineOptions {
        workers: dfdu_engine::workers_for(storage),
        ..opts
    };
    let engine = Engine::start(native_backend(storage, false), opts);
    let job = JobOptions {
        class: rules.map(|r| ClassOpts::for_root(r, path)),
        ..JobOptions::default()
    };
    (engine, job)
}

/// The built-in rules with the user's rule files. A rule that does not load is reported and
/// left out.
fn load_rules() -> Arc<RuleSet> {
    let dir = dfdu_sys::dirs::rules_dir();
    let (set, errors) = dfdu_rules::load(&dfdu_rules::Env::current(), dir.as_deref());
    for e in errors {
        eprintln!("dfdu-gui: {e}");
    }
    Arc::new(set)
}

/// Where the scan of `path` is cached; `None` when the cache is off, refused (said on standard
/// error), or `path` is not a directory (the scan then says why).
fn cache_slot(path: &Path) -> Option<dfdu_store::cache::Slot> {
    let cache = match dfdu_store::cache::Cache::open_default()? {
        Ok(cache) => cache,
        Err(e) => {
            eprintln!("dfdu-gui: {e}; the scan cache is not used");
            return None;
        }
    };
    let key = dfdu_store::cache::Key::new(path, &JobOptions::default(), false).ok()?;
    Some(cache.slot(key))
}

/// The tree of a scan saved in `file`: a snapshot, or an ncdu export.
///
/// # Errors
///
/// The file cannot be read or is not a scan.
pub fn import(file: &Path) -> io::Result<dfdu_tree::Tree> {
    let f = std::fs::File::open(file)?;
    let opened = dfdu_store::open_file(f, &dfdu_store::Limits::default())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(opened.tree)
}
