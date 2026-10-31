//! Scanner state shared by workers, the watchdog and the applier. The worker step function is
//! in the `worker` submodule, the watchdog's in `watchdog`, each worker's own state in `slot`,
//! the applier's control of jobs in `jobs`, the tasks the workers run in `task` and the limits
//! of the mounts the scan entered in `mounts`.

use fixedbitset::FixedBitSet;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use crate::backend::Backend;
use crate::classify::ClassOpts;
use crate::proto::{Batch, DirId, JobId, ScanMsg};
use crate::sync::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Injector, Mutex, RwLock};

mod jobs;
mod mounts;
mod slot;
mod task;
mod verify;
mod watchdog;
mod worker;

#[cfg(test)]
mod test_util;

use mounts::Gates;
pub use mounts::{MAX_MOUNT_WORKERS, TaskGate};
pub use slot::{Outbox, WorkerSlot};
pub use task::Task;
use task::{ChunkWork, DirParts, LISTING_PART, TaskKind};
pub use verify::VerifyDir;
pub use watchdog::WatchdogReport;
pub use worker::{RunningPart, StepResult, Worker};

/// Monotonic time in nanoseconds; never returns 0 (0 means "not set" in the slot fields).
pub trait Clock: Sync {
    fn now(&self) -> u64;
}

/// Where flushed batches go: the applier's inbox.
pub trait Sink: Sync {
    /// Sends without blocking.
    ///
    /// # Errors
    ///
    /// Returns the batch when the byte budget is exhausted.
    fn try_send(&self, batch: Batch) -> Result<(), Batch>;
    /// Sends, blocking until the byte budget allows it.
    fn send(&self, batch: Batch);
    /// Small control messages, exempt from the byte budget.
    fn send_control(&self, msg: ScanMsg);
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub block_entries: usize,
    pub block_name_bytes: usize,
    pub outbox_bytes: usize,
    pub outbox_msgs: usize,
    pub flush_age_ns: u64,
    pub stall_ns: u64,
    /// Chunk tasks a directory may have outstanding; 0 never hands a block to a chunk task. The
    /// engine sets it to `max(2, workers)` when it is 0.
    pub chunk_credit: usize,
    /// The pool's worker count, which the thread policy chose for the scan root's storage. A
    /// job root's mount whose storage has a limit (below) gets this many permits instead; 0
    /// gives it its storage's limit. The engine sets it to its worker count when it is 0.
    pub pool_workers: usize,
    /// Parts that may run at once on a mount on a spinning disk, and on a network mount. 0 is
    /// no limit. Mounts on other storage have none.
    pub rotational_parts: usize,
    pub network_parts: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            block_entries: 4096,
            block_name_bytes: 512 * 1024,
            outbox_bytes: 1024 * 1024,
            outbox_msgs: 256,
            flush_age_ns: 20_000_000,
            stall_ns: 10_000_000_000,
            chunk_credit: 0,
            pool_workers: 0,
            rotational_parts: 2,
            network_parts: 4,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct JobOptions {
    /// Descend into directories on other filesystems.
    pub cross_mounts: bool,
    /// Descend into network filesystems (NFS, SMB, sshfs and the like).
    pub network: bool,
    /// Mounts to descend into whatever `cross_mounts` and `network` say, by the device and
    /// inode of their root directories: those the user entered this session. Never kernel
    /// filesystems.
    pub enter: Vec<(u64, u64)>,
    /// The directory whose mounts are checked for duplicates: the visible scan root when a
    /// rescan covers only part of it. Defaults to the job root.
    pub scope: Option<PathBuf>,
    /// Classify entries against these rules while scanning (`crate::classify`).
    pub class: Option<ClassOpts>,
    /// Directories not read but reported as [`SkipReason::Kept`](crate::SkipReason::Kept): a
    /// resume's, which keeps them from the tree it loaded.
    pub keep: Option<Arc<crate::journal::Keep>>,
}

struct JobState {
    ended: AtomicBool,
    next_dir: AtomicU32,
    early_stop: RwLock<FixedBitSet>,
    /// With `cross_mounts`: mount ids not to descend into, from the mount table at job start.
    dup_mounts: Option<HashSet<u64>>,
    /// Without a mount table: `(dev, ino)` of every mount root descended into, the job root
    /// included, so at least a second mount seen later and mount loops are not followed.
    mount_roots: Mutex<HashSet<(u64, u64)>>,
    opts: JobOptions,
}

impl JobState {
    fn new(opts: JobOptions, dup_mounts: Option<HashSet<u64>>) -> Self {
        JobState {
            ended: AtomicBool::new(false),
            next_dir: AtomicU32::new(1),
            early_stop: RwLock::new(FixedBitSet::new()),
            dup_mounts,
            mount_roots: Mutex::new(HashSet::new()),
            opts,
        }
    }
    fn stopped(&self, dir: DirId) -> bool {
        self.early_stop.read().contains(dir.0 as usize)
    }
    fn stop(&self, dir: DirId) {
        let mut v = self.early_stop.write();
        v.grow(dir.0 as usize + 1);
        v.insert(dir.0 as usize);
    }
}

const NO_TASK: u64 = u64::MAX;

fn pack(job: JobId, dir: DirId) -> u64 {
    (u64::from(job.0) << 32) | u64::from(dir.0)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "splits the two halves `pack` joined"
)]
fn unpack(v: u64) -> (JobId, DirId) {
    (JobId((v >> 32) as u32), DirId(v as u32))
}

pub struct Shared<B: Backend> {
    pub backend: B,
    pub limits: Limits,
    injector: Injector<Task<B::Loc>>,
    priority: Injector<Task<B::Loc>>,
    outstanding: AtomicU64,
    outstanding_abandoned: AtomicU64,
    submitted_seq: AtomicU64,
    last_quiescent: AtomicU64,
    jobs: RwLock<HashMap<u32, Arc<JobState>>>,
    /// Parts `(job, dir, part)` the applier abandoned while a worker was running them.
    abandoned: Mutex<HashSet<(JobId, DirId, u32)>>,
    /// Blocks handed to chunk tasks, all jobs.
    pub chunk_tasks: AtomicU64,
    focus: AtomicU64,
    focus_epoch: AtomicU32,
    pub idle: AtomicUsize,
    slots: RwLock<Vec<Arc<WorkerSlot<B::Loc>>>>,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Limits by mount key (`mounts::mount_key`); `None` for mounts without one. Looked up when a
    /// task for a mount root is created.
    gates: Mutex<Gates<B::Loc>>,
    /// Workers the pool adds for limited mounts other than job roots'.
    mount_workers: AtomicUsize,
}

impl<B: Backend> Shared<B> {
    pub fn new(backend: B, limits: Limits) -> Self {
        Shared {
            backend,
            limits,
            injector: Injector::new(),
            priority: Injector::new(),
            outstanding: AtomicU64::new(0),
            outstanding_abandoned: AtomicU64::new(0),
            submitted_seq: AtomicU64::new(0),
            last_quiescent: AtomicU64::new(u64::MAX),
            jobs: RwLock::new(HashMap::new()),
            abandoned: Mutex::new(HashSet::new()),
            chunk_tasks: AtomicU64::new(0),
            focus: AtomicU64::new(NO_TASK),
            focus_epoch: AtomicU32::new(1),
            idle: AtomicUsize::new(0),
            slots: RwLock::new(Vec::new()),
            waker: Mutex::new(None),
            gates: Mutex::new(HashMap::new()),
            mount_workers: AtomicUsize::new(0),
        }
    }

    /// Called after tasks are published, so idle threads can be woken (real runtime only).
    pub fn set_waker(&self, f: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(f);
    }

    fn wake(&self) {
        if let Some(f) = &*self.waker.lock() {
            f();
        }
    }

    pub fn new_slot(&self) -> Arc<WorkerSlot<B::Loc>> {
        let s = Arc::new(WorkerSlot::new());
        self.slots.write().push(s.clone());
        s
    }

    pub fn remove_slot(&self, slot: &Arc<WorkerSlot<B::Loc>>) {
        self.slots.write().retain(|s| !Arc::ptr_eq(s, slot));
    }

    /// What every worker slot is doing and which parts are abandoned, for diagnosing a run
    /// that does not reach quiescence.
    pub fn debug_parts(&self) -> String {
        let mut out = format!(
            "abandoned {:?}; injector {} priority {}\n",
            self.abandoned.lock(),
            self.injector.len(),
            self.priority.len()
        );
        for g in self.gate_stats() {
            let _ = writeln!(out, "gate {g:?}");
        }
        for (i, slot) in self.slots.read().iter().enumerate() {
            let cur = slot.current.load(SeqCst);
            let ob = slot.outbox.lock();
            let _ = writeln!(
                out,
                "slot {i}: current {:?} part {} since {} tasks {:?} done {:?} msgs {}",
                (cur != NO_TASK).then(|| unpack(cur)),
                slot.part.load(SeqCst),
                slot.syscall_since.load(SeqCst),
                ob.tasks,
                ob.done,
                ob.msgs.len()
            );
        }
        out
    }

    /// Published tasks not yet taken: `(priority, normal)`.
    pub fn queued(&self) -> (usize, usize) {
        (self.priority.len(), self.injector.len())
    }

    /// Whether a published task is waiting to be taken.
    pub fn has_queued(&self) -> bool {
        !self.priority.is_empty() || !self.injector.is_empty()
    }

    /// Tasks not yet done (queued or running), and how many of those are parts the applier
    /// abandoned: `(outstanding, abandoned)`.
    pub fn outstanding(&self) -> (u64, u64) {
        (
            self.outstanding.load(SeqCst),
            self.outstanding_abandoned.load(SeqCst),
        )
    }

    /// Sends the outbox and then publishes its tasks, under the caller's outbox lock.
    /// Returns false if `blocking` is false and the budget is exhausted.
    fn flush_locked(&self, ob: &mut Outbox<B::Loc>, sink: &dyn Sink, blocking: bool) -> bool {
        if !ob.msgs.is_empty() {
            let batch = Batch {
                msgs: std::mem::take(&mut ob.msgs),
                bytes: ob.bytes,
            };
            if blocking {
                sink.send(batch);
            } else if let Err(b) = sink.try_send(batch) {
                ob.msgs = b.msgs;
                return false;
            }
            ob.bytes = 0;
        }
        let published = !ob.tasks.is_empty();
        let epoch = self.focus_epoch.load(SeqCst);
        for t in ob.tasks.drain(..) {
            if t.focus_epoch == epoch {
                self.priority.push(t);
            } else {
                self.injector.push(t);
            }
        }
        ob.has_chunk_task = false;
        ob.gated_task = false;
        if !ob.done.is_empty() {
            let mut ab = self.abandoned.lock();
            for c in ob.done.drain(..) {
                if ab.remove(&c) {
                    self.outstanding_abandoned.fetch_sub(1, SeqCst);
                }
                self.outstanding.fetch_sub(1, SeqCst);
            }
        }
        if published {
            self.wake();
        }
        true
    }
}

/// The message of a caught panic payload (`panic!` with a literal or a formatted string).
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(std::string::ToString::to_string)
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}
