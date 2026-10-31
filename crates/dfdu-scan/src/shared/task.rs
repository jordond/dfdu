//! The tasks the workers run: a directory's listing task, the chunk tasks it hands blocks to,
//! and the batches of a resume's check, with the parts of a directory read by more than one
//! of them.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use super::TaskGate;
use super::verify;
use crate::backend::Raw;
use crate::classify::{Listing, TaskClass};
use crate::proto::{DirId, JobId, ListingStatus};
use crate::sync::{AtomicU32, Mutex};

pub struct Task<L> {
    pub job: JobId,
    pub dir: DirId,
    pub loc: L,
    pub(super) focus_epoch: u32,
    pub(super) is_root: bool,
    /// The directory's own `dev` and mount id (the root's are filled in when it is opened).
    pub(super) dev: u64,
    pub(super) mount: u64,
    /// Ask the backend for a subvolume record when opening (`dev` changed from the parent).
    pub(super) probe_subvol: bool,
    /// What the task does in its directory.
    pub(super) kind: TaskKind<L>,
    /// The limit of the directory's mount, if it has one. Set for a job root once it is opened;
    /// the root task itself runs without a permit.
    pub(super) gate: Option<Arc<TaskGate<L>>>,
    /// What the directory's mount is stored on, as the backend reported it when the scan
    /// entered the mount; handed to [`Backend::open`](crate::Backend::open). A directory
    /// inherits its parent's.
    pub(super) storage: dfdu_sys::Storage,
    /// What the directory's entries are classified against, when the job classifies.
    pub(super) class: Option<Box<TaskClass>>,
}

/// What a [`Task`] does in its directory.
pub(super) enum TaskKind<L> {
    /// The directory's listing task.
    Listing,
    /// A chunk task: one block of the directory to stat.
    Chunk(Box<ChunkWork<L>>),
    /// A batch of a resume's check of the directories it keeps
    /// ([`Shared::verify`](super::Shared::verify)): no directory is listed, each of the
    /// batch's is stat'ed.
    Verify(Box<verify::VerifyWork>),
}

impl<L> TaskKind<L> {
    /// The block of a chunk task.
    pub(super) fn chunk(&self) -> Option<&ChunkWork<L>> {
        match self {
            TaskKind::Chunk(work) => Some(work),
            TaskKind::Listing | TaskKind::Verify(_) => None,
        }
    }

    pub(super) fn chunk_mut(&mut self) -> Option<&mut ChunkWork<L>> {
        match self {
            TaskKind::Chunk(work) => Some(work),
            TaskKind::Listing | TaskKind::Verify(_) => None,
        }
    }

    /// The directories of a batch task and what it found so far.
    pub(super) fn verify_mut(&mut self) -> Option<&mut verify::VerifyWork> {
        match self {
            TaskKind::Verify(work) => Some(work),
            TaskKind::Listing | TaskKind::Chunk(_) => None,
        }
    }

    /// Takes the work out of a task that is ending, leaving a listing task's kind.
    pub(super) fn take(&mut self) -> TaskKind<L> {
        std::mem::replace(self, TaskKind::Listing)
    }
}

impl<L: std::fmt::Debug> std::fmt::Debug for Task<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("job", &self.job)
            .field("dir", &self.dir)
            .field("loc", &self.loc)
            .field("chunk", &self.kind.chunk().map(|c| c.seq))
            .finish_non_exhaustive()
    }
}

/// A block handed from a directory's listing task to a chunk task.
pub struct ChunkWork<L> {
    pub(super) raw: Raw<L>,
    pub(super) seq: u32,
    pub(super) parts: Arc<DirParts>,
    pub(super) class: Option<Arc<Listing>>,
}

/// The parts of a directory read by more than one task: its listing task and the chunk tasks it
/// handed blocks to. Whoever finishes the last part sends the directory's one `Close`, with
/// every part's outcome in its status.
#[derive(Default)]
pub struct DirParts {
    /// The listing task (until it finishes) plus chunk tasks not finished.
    pub(super) unfinished: AtomicU32,
    /// Chunk messages of the directory, all parts; set by the listing task when it finishes.
    pub(super) chunks: AtomicU32,
    pub(super) panicked: AtomicU32,
    pub(super) cancelled: AtomicU32,
    /// The listing task's own outcome, set when it finishes.
    pub(super) listing: Mutex<Option<ListingStatus>>,
}

impl DirParts {
    pub(super) fn new() -> Self {
        DirParts {
            unfinished: AtomicU32::new(1),
            ..Default::default()
        }
    }

    /// Chunk tasks handed out and not finished.
    pub(super) fn in_flight(&self) -> usize {
        self.unfinished.load(SeqCst).saturating_sub(1) as usize
    }

    /// The directory's status once every part has finished.
    pub(super) fn status(&self) -> ListingStatus {
        let (mut panicked, mut cancelled, mut read_error) = (
            self.panicked.load(SeqCst),
            self.cancelled.load(SeqCst),
            None,
        );
        match self.listing.lock().clone() {
            Some(ListingStatus::Partial {
                panicked: p,
                cancelled: c,
                read_error: e,
            }) => {
                panicked += p;
                cancelled += c;
                read_error = e;
            }
            // Parts exist only once entries were read, so the listing did not fail or end
            // before its first entry.
            Some(ListingStatus::Cancelled) => cancelled += 1,
            Some(ListingStatus::Failed(e)) => read_error = Some(e),
            Some(ListingStatus::Complete) | None => {}
        }
        if panicked == 0 && cancelled == 0 && read_error.is_none() {
            ListingStatus::Complete
        } else {
            ListingStatus::Partial {
                panicked,
                cancelled,
                read_error,
            }
        }
    }
}

/// Part number of a directory's listing task; chunk task `seq` is part `seq + 1`.
pub(super) const LISTING_PART: u32 = 0;

#[cfg(test)]
mod tests;
