//! The engine: the applier thread that is the single writer of the tree, and the API the TUI
//! and the headless commands use.

mod applier;
mod classifier;
mod extents;
mod journal;
mod links;
mod measure;
mod recent;
mod reclassify;
mod remote;
mod runtime;

pub use applier::{
    Applier, Command, Count, DeleteProgress, DeleteSummary, Estimate, JobInfo, JobKind, JobPhase,
    MeasureProgress, Model, NATIVE_LINES, NativeRun, StalledInfo, Stats, Status, Timing, UndoInfo,
    native_argv,
};
pub use classifier::{
    CheckMsg, CheckReq, check_batch, git_tracked, parse_reclaimable, spawn_classifiers,
};
// Engine callers take these from here and use dfdu_clean only for what is not re-exported.
pub use dfdu_clean::{DeleteResult, Mode, STAGE_UNDO_WINDOW, shown_argv};
pub use extents::{Inode, Measurement};
pub use journal::{changed_since, journal_position};
pub use measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
#[cfg(target_os = "linux")]
pub use measure::{from_extents as measure_from_extents, from_items as measure_from_items};
pub use recent::{Event, RECENT, Recent};
pub use remote::{Connected, DEAD, RemoteIo, RemoteRules, agent, wire};
pub use runtime::{
    Engine, EngineOptions, WaitIdle, crash_state, default_crash_dir, panic_is_caught_by_engine,
    workers_for, write_crash_report,
};
