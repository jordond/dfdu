//! Deletion (`docs/data-pipeline.md` §7.4, §9.2): plans built by the applier, verification of
//! every item against the scan, trash or permanent delete, and a per-session journal whose
//! intent record reaches the disk before an item is touched.

mod deleter;
mod journal;
mod remove;
mod verify;

use std::path::PathBuf;

use dut_tree::{ErrKind, Kind, NodeRef};

pub use deleter::{DeleterHandle, spawn_deleter};
pub use journal::{Journal, check_line, default_journal_dir};
pub use remove::remove;
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
    /// The item is in another btrfs subvolume than the trash; a rename between subvolumes is
    /// not possible, so the trash would copy it. Holds the root of the item's subvolume.
    TrashOtherSubvolume(PathBuf),
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
        /// Something was deleted on btrfs, which frees the space when the transaction commits
        /// (deleted snapshots later, in the background).
        btrfs: bool,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Protected => "refused: a protected system location",
            Refusal::OutsideScanRoot => "refused: outside the scanned directory",
            Refusal::ScanRoot => "refused: the scanned directory itself",
            Refusal::MountPoint => "refused: a mount point",
            Refusal::ReadOnly => "refused: read-only mode",
            Refusal::TrashCrossesFilesystems => {
                "refused for trash: the trash is on another filesystem (it would be copied)"
            }
            Refusal::TrashOtherSubvolume(sv) => {
                return write!(
                    f,
                    "refused for trash: the item is in btrfs subvolume {}, the trash in another \
                     (it would be copied)",
                    sv.display()
                );
            }
            Refusal::NoJournal => "refused: the deletion journal could not be written",
        })
    }
}

impl std::fmt::Display for DeleteResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteResult::Deleted => f.write_str("deleted"),
            DeleteResult::Stale(why) => write!(f, "not deleted: {why}"),
            DeleteResult::Refused(r) => write!(f, "{r}"),
            DeleteResult::Failed { partial, msg, .. } => {
                if *partial {
                    write!(f, "partly deleted: {msg}")
                } else {
                    write!(f, "failed: {msg}")
                }
            }
            DeleteResult::Aborted => f.write_str("not deleted: aborted"),
        }
    }
}
