//! A resume's check of the directories it keeps from the tree it loaded: each is stat'ed
//! again and compared with what was loaded, in batches run by the workers like any task.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use super::{JobState, Shared, Task, TaskKind};
use crate::backend::Backend;
use crate::proto::{DirId, JobId};

/// A directory a resume keeps, as it was loaded: its path (made a location by the worker,
/// [`Backend::stat_dir`], since making one can take a call), its device and inode, and its
/// mtime, `None` when that cannot vouch for its entries (it was changed in the second the
/// loaded tree's scan began or later, so a change in that same second after the scan read it
/// would leave it as it is).
#[derive(Clone, Debug)]
pub struct VerifyDir {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub mtime: Option<i64>,
}

/// A batch task's directories and what it found so far.
#[derive(Default)]
pub(super) struct VerifyWork {
    pub batch: u32,
    pub dirs: Vec<VerifyDir>,
    /// The next directory to stat.
    pub at: usize,
    pub changed: Vec<u32>,
    pub gone: Vec<u32>,
}

impl<B: Backend> Shared<B> {
    /// Queues a batch of directories to stat for the resume of `job` (a job with no scan of
    /// its own): each worker that takes it reports what it found in one
    /// [`ScanMsg::Verified`](crate::ScanMsg::Verified). [`Shared::end_job`] cancels the batches
    /// not yet stat'ed. The batches run without a mount's permit: they are of the root's
    /// device, whose limit is the pool's size. `root` is the resume's root, where the tasks say
    /// they are.
    pub fn verify(&self, job: JobId, batch: u32, root: B::Loc, dirs: Vec<VerifyDir>) {
        if dirs.is_empty() {
            return;
        }
        self.jobs
            .write()
            .entry(job.0)
            .or_insert_with(|| Arc::new(JobState::new(crate::JobOptions::default(), None)));
        self.outstanding.fetch_add(1, SeqCst);
        self.submitted_seq.fetch_add(1, SeqCst);
        self.injector.push(Task {
            job,
            dir: DirId(batch),
            loc: root,
            focus_epoch: 0,
            is_root: false,
            dev: 0,
            mount: 0,
            probe_subvol: false,
            kind: TaskKind::Verify(Box::new(VerifyWork {
                batch,
                dirs,
                at: 0,
                changed: Vec::new(),
                gone: Vec::new(),
            })),
            gate: None,
            storage: dfdu_sys::Storage::Unknown,
            class: None,
        });
        self.wake();
    }
}

#[cfg(test)]
mod tests;
