//! The engine: the applier thread that is the single writer of the tree, and the API the TUI
//! and the headless commands use (`docs/data-pipeline.md` §2, §5).

mod applier;
mod links;
mod runtime;

pub use applier::{
    Applier, Command, JobInfo, JobKind, JobPhase, Model, StalledInfo, Stats, Status, bytes_to_path,
    meta_of,
};
pub use runtime::{Engine, EngineOptions, Inbox};
