//! Scanner control: the local scanner, or the agent of a remote scan. Every call that steers
//! scan jobs goes through here, so none has to choose between the two itself.

use std::path::Path;

use dfdu_scan::{Backend, DirId, JobId, JobOptions};

use super::Applier;

impl<B: Backend> Applier<B> {
    /// Starts job `job` scanning `path`; returns the sequence number of its submission.
    pub(super) fn scanner_start_job(&self, job: JobId, path: &Path, opts: JobOptions) -> u64 {
        match &self.remote {
            Some(l) => l.start_job(job, path, &opts),
            None => self.scan.start_job(job, path, opts),
        }
    }

    pub(super) fn scanner_end_job(&self, job: JobId) {
        match &self.remote {
            Some(l) => l.end_job(job),
            None => self.scan.end_job(job),
        }
    }

    /// Workers skip these directories of `job`.
    pub(super) fn scanner_stop_dirs(&self, job: JobId, dirs: impl IntoIterator<Item = DirId>) {
        match &self.remote {
            Some(l) => l.stop_dirs(job, dirs),
            None => self.scan.stop_dirs(job, dirs),
        }
    }

    /// Abandons a stalled directory, to be retried from [`Applier::tick`] when a worker's
    /// outbox could not be locked. An agent retries on its side.
    pub(super) fn scanner_abandon(&mut self, job: JobId, dir: DirId) {
        if !self.scanner_try_abandon(job, dir) {
            self.retry_abandon.push((job, dir));
        }
    }

    /// Whether the abandon is complete.
    pub(super) fn scanner_try_abandon(&self, job: JobId, dir: DirId) -> bool {
        match &self.remote {
            Some(l) => l.abandon(job, dir),
            None => self.scan.abandon(job, dir),
        }
    }
}
