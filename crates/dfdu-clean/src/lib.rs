//! Deletion: plans built by the applier, verification of every item against the scan, trash,
//! staging or permanent delete, undo within the session, a per-session journal whose intent
//! record reaches the disk before an item is touched, and recovery of what an earlier session
//! left unfinished.

mod deleter;
pub mod inside;
mod journal;
mod native;
pub mod recover;
mod remove;
mod stage;
mod verify;

use std::path::PathBuf;

use dfdu_tree::{ErrKind, Kind, NodeRef};
use serde::{Deserialize, Serialize};

pub use deleter::{DeleterHandle, STAGE_UNDO_WINDOW, spawn_deleter};
pub use journal::{Journal, check_line, default_journal_dir};
pub use native::{NativeReq, NativeRule, shown as shown_argv};
pub use remove::{remove, remove_verified};
pub use stage::rename_noreplace;
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

/// How a plan deletes. Journal intent records name it `trash`, `stage` or `delete`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Trash,
    /// Renamed into a staging directory on the item's filesystem, which takes no time, and
    /// deleted in the background once the undo window has passed.
    Stage,
    #[serde(rename = "delete")]
    Permanent,
}

impl Mode {
    /// Its name in the journal.
    pub(crate) fn journal_name(self) -> &'static str {
        match self {
            Mode::Trash => "trash",
            Mode::Stage => "stage",
            Mode::Permanent => "delete",
        }
    }

    /// Whether the session can put an item back.
    pub fn undoable(self) -> bool {
        matches!(self, Mode::Trash | Mode::Stage)
    }
}

/// What the deleter thread is asked to do.
#[derive(Clone, Debug)]
pub enum DeleterReq {
    Plan(Plan),
    /// Puts back what the plan moved to the trash or staged, where still possible.
    Undo {
        plan: u64,
    },
    /// Deletes every staged item now instead of when its undo window ends.
    Commit,
    /// Runs a native cleanup command, or its dry run.
    Native(NativeReq),
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub node: NodeRef,
    pub path: PathBuf,
    pub identity: Identity,
    /// Disk usage of the item as scanned, for progress and reporting.
    pub bytes: u64,
    /// For a directory, what the scan saw in it, compared before it is deleted.
    pub inside: std::sync::Arc<inside::Listings>,
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
    /// A filesystem is mounted inside the directory; moving it would move the mount.
    ContainsMount(PathBuf),
    ReadOnly,
    /// Moving to the trash would copy the item to another filesystem (Linux home trash).
    TrashCrossesFilesystems,
    /// The item is in another btrfs subvolume than the trash; a rename between subvolumes is
    /// not possible, so the trash would copy it. Holds the root of the item's subvolume.
    TrashOtherSubvolume(PathBuf),
    /// There is no directory for the deletion journal, so nothing is deleted. When the journal
    /// cannot be created there, each item fails with the cause instead.
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
    /// A staged item was deleted in the background (or that failed, and it stays staged for
    /// recovery).
    Committed {
        plan: u64,
        bytes: u64,
        result: Result<(), String>,
    },
    /// An undo put an item back at the path in `result` (`original`, or `<name>.restored-<n>`
    /// next to it when something else is there now), or could not.
    Restored {
        plan: u64,
        original: PathBuf,
        /// It was staged (not in the trash), and its disk usage as scanned.
        staged: bool,
        bytes: u64,
        result: Result<PathBuf, String>,
    },
    /// Every item of an undo was answered.
    UndoDone {
        plan: u64,
    },
    /// A line a native command wrote (stdout or stderr), or the command line before it runs.
    NativeOutput {
        id: u64,
        line: String,
    },
    NativeDone {
        id: u64,
        result: Result<(), String>,
    },
    /// What exited sessions left unfinished, found when the deleter starts
    /// ([`recover::Unfinished::describe`]), and notes on damaged or unreadable journals and
    /// on journal records that could not be written. Nothing is done with them here.
    Unfinished {
        items: Vec<String>,
        notes: Vec<String>,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Protected => "refused: a protected system location",
            Refusal::OutsideScanRoot => "refused: outside the scanned directory",
            Refusal::ScanRoot => "refused: the scanned directory itself",
            Refusal::MountPoint => "refused: a mount point",
            Refusal::ContainsMount(m) => {
                return write!(f, "refused: a filesystem is mounted at {}", m.display());
            }
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
