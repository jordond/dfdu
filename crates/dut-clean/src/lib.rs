//! Deletion (`docs/data-pipeline.md` §7.4, §9.2): plans built by the applier, verification of
//! every item against the scan, trash or permanent delete, and a per-session journal whose
//! intent record reaches the disk before an item is touched.

mod crc;
mod deleter;
mod journal;
mod verify;

use std::path::PathBuf;

use dut_tree::{ErrKind, Kind, NodeRef};

pub use deleter::{DeleterHandle, spawn_deleter};
pub use journal::{Journal, check_line, default_journal_dir};
pub use verify::{is_protected, verify};

/// What the scan recorded about an item; deletion proceeds only if the filesystem still agrees.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub kind: Kind,
    pub apparent: u64,
    pub mtime: i64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Trash,
    Permanent,
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub node: NodeRef,
    pub path: PathBuf,
    pub identity: Identity,
    /// Disk usage of the item as scanned, for progress and reporting.
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub id: u64,
    pub mode: Mode,
    /// Items must be strictly inside this directory.
    pub scan_root: PathBuf,
    pub items: Vec<PlanItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Protected,
    OutsideScanRoot,
    ScanRoot,
    MountPoint,
    ReadOnly,
    /// Moving to the trash would copy the item to another filesystem (Linux home trash).
    TrashCrossesFilesystems,
    /// The deletion journal could not be written, so nothing is deleted.
    NoJournal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteResult {
    Deleted,
    /// Changed on disk since the scan (or gone); not touched.
    Stale(String),
    Refused(Refusal),
    Failed {
        partial: bool,
        err: ErrKind,
        msg: String,
    },
    Aborted,
}

#[derive(Clone, Debug)]
pub enum DeleterMsg {
    Item {
        plan: u64,
        node: NodeRef,
        bytes: u64,
        result: DeleteResult,
    },
    Done {
        plan: u64,
    },
}
