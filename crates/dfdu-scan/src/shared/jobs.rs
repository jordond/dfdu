//! The applier's control of the scanner's jobs: starting and ending them, stopping and
//! abandoning their directories, and the focus.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use super::{JobOptions, JobState, Shared, Task, pack};
use crate::backend::Backend;
use crate::classify::TaskClass;
use crate::proto::{DirId, JobId};

impl<B: Backend> Shared<B> {
    pub(super) fn job(&self, job: JobId) -> Option<Arc<JobState>> {
        self.jobs.read().get(&job.0).cloned()
    }

    /// Submits the root task of a job. The applier creates the root placeholder first. Returns
    /// the `submitted_seq` value that covers this job.
    pub fn start_job(&self, job: JobId, root: &Path, opts: JobOptions) -> u64 {
        let dup_mounts = (opts.cross_mounts || opts.network)
            .then(|| {
                self.backend
                    .duplicate_mounts(opts.scope.as_deref().unwrap_or(root))
            })
            .flatten();
        let st = Arc::new(JobState::new(opts, dup_mounts));
        self.jobs.write().insert(job.0, st);
        self.outstanding.fetch_add(1, SeqCst);
        let seq = self.submitted_seq.fetch_add(1, SeqCst) + 1;
        let loc = self.backend.loc_from_path(root);
        let class = self
            .job(job)
            .and_then(|j| j.opts.class.as_ref().map(TaskClass::root))
            .map(Box::new);
        self.priority.push(Task {
            job,
            dir: DirId::ROOT,
            loc,
            focus_epoch: 0,
            is_root: true,
            dev: 0,
            mount: 0,
            probe_subvol: true,
            chunk: None,
            verify: None,
            gate: None,
            storage: dfdu_sys::Storage::Unknown,
            class,
        });
        self.wake();
        seq
    }

    /// Ends a job (finished or cancelled). Tasks still queued for it close as cancelled.
    pub fn end_job(&self, job: JobId) {
        if let Some(st) = self.jobs.write().remove(&job.0) {
            st.ended.store(true, SeqCst);
        }
    }

    /// Workers skip these directories (an optimisation; the applier's gate decides).
    pub fn stop_dirs(&self, job: JobId, dirs: impl IntoIterator<Item = DirId>) {
        if let Some(st) = self.job(job) {
            for d in dirs {
                st.stop(d);
            }
        }
    }

    /// Records that the applier abandoned `(job, dir)`: every part of it a worker is running
    /// stops counting toward quiescence. Returns `false` if it could not take the outbox lock
    /// of a worker running a part without blocking; the applier retries on its next slice, and
    /// parts already recorded are not counted twice. Parts no worker is running are either
    /// finished (their completion is on the way) or queued (they will see the stop bit and
    /// finish at once).
    pub fn abandon(&self, job: JobId, dir: DirId) -> bool {
        self.stop_dirs(job, [dir]);
        let target = pack(job, dir);
        let mut complete = true;
        for slot in self.slots.read().iter() {
            let Some(_ob) = slot.outbox.try_lock() else {
                if slot.current.load(SeqCst) == target {
                    complete = false;
                }
                continue;
            };
            if slot.current.load(SeqCst) == target
                && self
                    .abandoned
                    .lock()
                    .insert((job, dir, slot.part.load(SeqCst)))
            {
                self.outstanding_abandoned.fetch_add(1, SeqCst);
            }
        }
        complete
    }

    /// Makes `(job, dir)` the focus. The focus is stored before the epoch changes; readers load
    /// the epoch first (`Worker::emit_block`).
    pub fn set_focus(&self, job: JobId, dir: DirId) {
        self.focus.store(pack(job, dir), SeqCst);
        self.focus_epoch.fetch_add(1, SeqCst);
    }
}

#[cfg(test)]
mod tests;
