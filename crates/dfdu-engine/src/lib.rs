//! The engine: the applier thread that is the single writer of the tree, and the API the TUI
//! and the headless commands use (`docs/data-pipeline.md` §2, §5).

mod applier;
mod extents;
mod links;
mod measure;
mod recent;
mod runtime;

pub use applier::{
    Applier, Command, DeleteProgress, DeleteSummary, JobInfo, JobKind, JobPhase, MeasureProgress,
    Model, StalledInfo, Stats, Status, Timing, meta_of,
};
pub use dfdu_clean::{DeleteResult, Mode};
pub use extents::{ExtentTable, Inode, Measurement};
pub use measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
#[cfg(target_os = "linux")]
pub use measure::{from_extents as measure_from_extents, from_items as measure_from_items};
pub use recent::{Event, RECENT, Recent};
pub use runtime::{
    Engine, EngineOptions, Inbox, crash_details, crash_state, default_crash_dir,
    panic_is_caught_by_engine, workers_for, write_crash_report,
};
