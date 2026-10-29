//! The engine: the applier thread that is the single writer of the tree, and the API the TUI
//! and the headless commands use, with what both show of the model alike: sizes, problems,
//! subvolume labels, the matches of the rules listed and which are recommended for deletion.

mod applier;
mod classifier;
mod crash;
mod extents;
mod links;
mod listed;
mod measure;
mod nanos;
mod notes;
mod recent;
mod reclassify;
mod recommend;
mod remote;
mod runtime;
mod sizes;
#[cfg(test)]
mod test_util;
mod threads;

pub use applier::{
    Applier, Command, Count, DeleteProgress, DeleteSummary, Estimate, JobInfo, JobKind, JobPhase,
    MeasureProgress, Model, NATIVE_LINES, NativeRun, StalledInfo, Stats, Status, Timing, UndoInfo,
    native_argv,
};
pub use classifier::{
    CheckMsg, CheckReq, check_batch, git_tracked, parse_reclaimable, spawn_classifiers,
};
// Engine callers take these from here and use dfdu_clean only for what is not re-exported.
pub use crash::{crash_state, default_crash_dir, panic_is_caught_by_engine, write_crash_report};
pub use dfdu_clean::{DeleteResult, Mode, STAGE_UNDO_WINDOW, shown_argv};
pub use dfdu_scan::journal::{JournalPosition, changed_since, journal_position};
pub use extents::{Inode, Measurement};
pub use listed::{Listed, listed};
pub use measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
#[cfg(target_os = "linux")]
pub use measure::{from_extents as measure_from_extents, from_items as measure_from_items};
pub use notes::{ErrorItem, ErrorList, errors, errors_below, problem, subvol_label};
pub use recent::{Event, RECENT, Recent};
pub use recommend::recommends;
pub use remote::{Connected, DEAD, RemoteIo, RemoteRules, agent, wire};
pub use runtime::{Engine, EngineOptions, WaitIdle, workers_for};
pub use sizes::{SizeMode, human};
