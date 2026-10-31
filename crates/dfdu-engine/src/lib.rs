//! The engine: the applier thread that is the single writer of the tree, and the API the TUI
//! and the headless commands use, with what both show of the model alike: sizes, problems,
//! subvolume labels, the matches of the rules listed and which are recommended for deletion,
//! the mounts the scan did not enter and the rule locations behind links out of it.

mod applier;
mod classifier;
mod crash;
mod extents;
mod measure;
mod nanos;
mod recent;
mod remote;
mod runtime;
mod shown;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
mod threads;

pub use applier::{
    Applier, ApplierSettings, Command, Count, DeleteProgress, DeleteSummary, Estimate, JobInfo,
    JobKind, JobPhase, MeasureProgress, Model, NATIVE_LINES, NativeRun, StalledInfo, Stats, Status,
    Timing, UndoInfo, native_argv,
};
pub use classifier::{
    CheckMsg, CheckReq, check_batch, git_tracked, parse_reclaimable, spawn_classifiers,
};
// Engine callers take these from here and use dfdu_clean only for what is not re-exported.
pub use crash::{crash_state, panic_is_caught_by_engine, write_crash_report};
pub use dfdu_clean::{DeleteResult, Mode, STAGE_UNDO_WINDOW, shown_argv};
pub use dfdu_scan::journal::{
    JournalPosition, changed_since, changed_since_cancellable, journal_position,
};
pub use extents::{Inode, Measurement};
pub use measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
#[cfg(target_os = "linux")]
pub use measure::{from_extents as measure_from_extents, from_items as measure_from_items};
pub use recent::{Event, RECENT, Recent};
pub use remote::{Connected, DEAD, RemoteIo, RemoteRules, agent, wire};
pub use runtime::{Engine, EngineOptions, JournalQuery, WaitIdle, workers_for};
pub use shown::{
    ErrorItem, ErrorList, LIST_MAX, Listed, NotEntered, SizeMode, Verdict, describe, errors,
    errors_below, home_dir, human, linked_note, listed, more_links, not_entered, not_entered_lines,
    problem, rel_text, subvol_label, tilde, verdict,
};
