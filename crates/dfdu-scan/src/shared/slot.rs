//! Each worker's own state: its outbox, and the slot the watchdog and the applier read.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use super::{LISTING_PART, NO_TASK, Task, TaskGate, unpack};
use crate::proto::{DirId, JobId, ScanMsg};
use crate::sync::{AtomicBool, AtomicU32, AtomicU64, Mutex};

#[cfg(test)]
mod tests;

/// Unsent messages and unpublished tasks of one worker. Held under the slot's mutex only to
/// append, pop or flush; never across a filesystem call.
pub struct Outbox<L> {
    pub(super) msgs: Vec<ScanMsg>,
    pub(super) bytes: usize,
    pub(super) oldest: u64,
    /// Parts (tasks) finished since the last flush: `(job, dir, part)`.
    pub(super) done: Vec<(JobId, DirId, u32)>,
    pub(super) tasks: Vec<Task<L>>,
    /// A chunk task is among `tasks`: publish it soon, others may be idle.
    pub(super) has_chunk_task: bool,
    /// An own task could not run for its mount's limit: publish it, so it can wait in the
    /// mount's gate and the tasks under it can be taken.
    pub(super) gated_task: bool,
    /// The gate whose permit the running part holds.
    pub(super) permit: Option<Arc<TaskGate<L>>>,
    /// A task handed over with a permit by the part that released it; runs next.
    pub(super) handoff: Option<Task<L>>,
}

impl<L> Default for Outbox<L> {
    fn default() -> Self {
        Outbox {
            msgs: Vec::new(),
            bytes: 0,
            oldest: 0,
            done: Vec::new(),
            tasks: Vec::new(),
            has_chunk_task: false,
            gated_task: false,
            permit: None,
            handoff: None,
        }
    }
}

impl<L> Outbox<L> {
    pub(super) fn push(&mut self, msg: ScanMsg, now: u64) {
        self.bytes += msg.size();
        if self.msgs.is_empty() {
            self.oldest = now;
        }
        self.msgs.push(msg);
    }
    /// Messages not yet sent.
    pub fn messages(&self) -> &[ScanMsg] {
        &self.msgs
    }

    /// Tasks not yet published.
    pub fn unpublished(&self) -> usize {
        self.tasks.len()
    }

    /// Nothing to send, publish or report done.
    pub fn is_empty(&self) -> bool {
        self.msgs.is_empty() && self.tasks.is_empty() && self.done.is_empty()
    }
}

/// Per-worker state visible to the watchdog and the applier.
pub struct WorkerSlot<L> {
    pub outbox: Mutex<Outbox<L>>,
    /// Time the worker entered its current filesystem call, or 0.
    pub(super) syscall_since: AtomicU64,
    /// Task being run, packed `(job, dir)`, or `NO_TASK`, and which part of the directory it
    /// is. Changed only under the outbox lock.
    pub(super) current: AtomicU64,
    pub(super) part: AtomicU32,
    pub(super) stall_reported: AtomicBool,
}

impl<L> WorkerSlot<L> {
    pub(super) fn new() -> Self {
        WorkerSlot {
            outbox: Mutex::new(Outbox::default()),
            syscall_since: AtomicU64::new(0),
            current: AtomicU64::new(NO_TASK),
            part: AtomicU32::new(LISTING_PART),
            stall_reported: AtomicBool::new(false),
        }
    }

    /// The part being run: `(job, dir, part)`, part 0 being the listing task and part
    /// `seq + 1` the chunk task for block `seq`.
    pub fn current(&self) -> Option<(JobId, DirId, u32)> {
        let cur = self.current.load(SeqCst);
        (cur != NO_TASK).then(|| {
            let (job, dir) = unpack(cur);
            (job, dir, self.part.load(SeqCst))
        })
    }

    /// Marks the start of a filesystem call. A call retried after "not ready" keeps its
    /// original start time.
    pub(super) fn begin_syscall(&self, now: u64) {
        let _ = self.syscall_since.compare_exchange(0, now, SeqCst, SeqCst);
    }

    /// Marks the end of a filesystem call, or of whatever call the part left open.
    pub(super) fn end_syscall(&self) {
        self.syscall_since.store(0, SeqCst);
    }

    /// Runs a filesystem call under the stall watch. A call that reports "not ready"
    /// (`WouldBlock`) stays open, so its retry keeps the original start time.
    pub(super) fn watch_io<T>(
        &self,
        now: u64,
        call: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.begin_syscall(now);
        let res = call();
        if !matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.end_syscall();
        }
        res
    }

    /// Runs a filesystem call that cannot report "not ready" under the stall watch.
    pub(super) fn watch<T>(&self, now: u64, call: impl FnOnce() -> T) -> T {
        self.begin_syscall(now);
        let res = call();
        self.end_syscall();
        res
    }
}
