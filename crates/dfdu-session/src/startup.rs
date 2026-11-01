//! What a front end opens (`Source`) and the parts of starting it that need no terminal or
//! window: resuming a scan read from a file (`resume`) and the link to the scan history its
//! scan is recorded in (`history`).

use std::path::{Path, PathBuf};

use dfdu_engine::{Engine, EngineOptions, JournalPosition};
use dfdu_scan::JobOptions;

use crate::history_link::HistoryLink;

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
        journal: Option<JournalPosition>,
        start: StartLocal,
    },
    /// A directory on this host, as [`Source::Local`], opened from its cached scan in `slot`
    /// when there is one (shown at once and brought up to date as [`Source::Resumed`] is), and
    /// whose complete scans are saved there.
    Cached {
        path: PathBuf,
        slot: dfdu_store::cache::Slot,
        start: StartLocal,
    },
    /// A scan read from a file, browsed read-only.
    Imported(dfdu_tree::Tree),
    /// An engine scanning `path` on another host with `job`: the root the agent resolved
    /// ([`dfdu_engine::Connected::root`]). It deletes there too (unless
    /// [`EngineOptions::read_only`]); nothing is recorded in the history.
    Remote {
        engine: Engine<dfdu_scan::NativeBackend>,
        path: dfdu_tree::HostPath,
        job: JobOptions,
    },
}

/// Starts an engine on the root of `tree`, loads the tree into it and rescans what changed
/// since `journal`, or all of it. Returns the engine with the root's path and the options of
/// the scan; `None` when the tree has no root.
#[must_use]
pub fn resume(
    tree: dfdu_tree::Tree,
    journal: Option<JournalPosition>,
    start: StartLocal,
    engine: &EngineOptions,
) -> Option<(Engine<dfdu_scan::NativeBackend>, PathBuf, JobOptions)> {
    let root = tree.roots().next()?;
    let path = tree.path_buf(root);
    let (engine, job) = start(&path, engine.clone());
    engine.resume(tree, journal, job.clone());
    Some((engine, path, job))
}

/// The link to the scan history a scan of `root` with `job`, by an engine with `engine`, is
/// recorded in, its last scan's sizes loading; `None` unless `record` and the history has a
/// place on this machine. Only scans that count the same data are compared.
#[must_use]
pub fn history(
    record: bool,
    root: &Path,
    job: &JobOptions,
    engine: &EngineOptions,
) -> Option<HistoryLink> {
    let db = dfdu_sys::dirs::history_path().filter(|_| record)?;
    let key = dfdu_store::history::options_key(job, engine.measure_workers > 0);
    Some(HistoryLink::start(db, root.to_path_buf(), key))
}
