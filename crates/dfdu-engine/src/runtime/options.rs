//! How an engine runs: its threads, budgets, journal and crash report directories.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dfdu_scan::Limits;

use crate::crash::default_crash_dir;

/// How an engine runs. [`Default`] reads nothing from the environment: no journal (deletion is
/// refused), no crash reports and no invariant checks. The commands start from
/// [`EngineOptions::from_env`].
#[derive(Clone, Debug)]
pub struct EngineOptions {
    /// Worker threads, chosen for the scan root's storage ([`workers_for`]). A scan that enters
    /// a mount with its own limit (a spinning disk, a network mount) adds that many workers,
    /// at most [`dfdu_scan::MAX_MOUNT_WORKERS`].
    pub workers: usize,
    pub budget_bytes: usize,
    pub limits: Limits,
    /// Abandon stalled directories automatically after this long (headless mode).
    pub auto_abandon: Option<Duration>,
    /// Check every invariant after every slice. Linear in tree size; for tests and debugging.
    pub check_invariants: bool,
    /// Where the deletion journal goes. Without one, deletion is refused.
    pub journal_dir: Option<PathBuf>,
    /// Refuse all deletion.
    pub read_only: bool,
    /// How long a staged item can be put back before it is deleted.
    pub stage_window: Duration,
    /// Where crash reports go.
    pub crash_dir: Option<PathBuf>,
    /// Threads measuring extents on filesystems that share them; 0 turns measurement off and
    /// sizes are what `du` reports.
    pub measure_workers: usize,
    /// Classifier threads checking matches against their guards (git tracking, running
    /// processes); 0 turns the checks off.
    pub classifiers: usize,
}

/// Worker threads for data on `storage`: two on a spinning disk, where more threads only add
/// seeks; otherwise one per core, 2 to 16. On a Mac with performance and efficiency cores,
/// one per performance core: on an M4 Max (12 + 4) a warm scan of 1M entries takes 2.0 s with
/// 10 or 12 workers, 2.5 s with 14 and 3.0 s with 16.
#[must_use]
pub fn workers_for(storage: dfdu_sys::Storage) -> usize {
    let all = || thread::available_parallelism().map_or(4, std::num::NonZero::get);
    #[cfg(target_os = "macos")]
    let cores = dfdu_sys::performance_cores().unwrap_or_else(all);
    #[cfg(not(target_os = "macos"))]
    let cores = all();
    match storage {
        dfdu_sys::Storage::Rotational => 2,
        _ => cores.clamp(2, 16),
    }
}

/// Measurer threads by default: none where nothing is measured.
pub(crate) fn default_measurers() -> usize {
    // The kernel's extent lookups are the cost, and they scale with threads.
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        thread::available_parallelism()
            .map_or(2, std::num::NonZero::get)
            .clamp(2, 8)
    } else {
        0
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            workers: workers_for(dfdu_sys::Storage::Unknown),
            budget_bytes: 64 * 1024 * 1024,
            limits: Limits::default(),
            auto_abandon: None,
            check_invariants: false,
            journal_dir: None,
            read_only: false,
            stage_window: dfdu_clean::STAGE_UNDO_WINDOW,
            crash_dir: None,
            measure_workers: default_measurers(),
            classifiers: 2,
        }
    }
}

impl EngineOptions {
    /// The defaults, with the deletion journal and crash reports in the user's state directory
    /// and invariant checks on when `DFDU_CHECK` is set.
    #[must_use]
    pub fn from_env() -> Self {
        EngineOptions {
            check_invariants: std::env::var_os("DFDU_CHECK").is_some(),
            journal_dir: dfdu_clean::default_journal_dir(),
            crash_dir: default_crash_dir(),
            ..EngineOptions::default()
        }
    }
}

/// The scan limits `opts` asks for, credit and pool sized for its workers where it leaves
/// them at 0.
pub(super) fn scan_limits(opts: &EngineOptions) -> Limits {
    let mut limits = opts.limits.clone();
    if limits.chunk_credit == 0 {
        limits.chunk_credit = opts.workers.max(2);
    }
    if limits.pool_workers == 0 {
        limits.pool_workers = opts.workers.max(1);
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::EngineOptions;

    #[test]
    fn default_options_read_nothing_from_the_environment() {
        let opts = EngineOptions::default();
        assert_eq!(opts.journal_dir, None);
        assert_eq!(opts.crash_dir, None);
        assert!(!opts.check_invariants);
    }
}
