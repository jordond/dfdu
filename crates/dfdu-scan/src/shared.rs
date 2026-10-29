//! Scanner state shared by workers, the watchdog and the applier. The worker step function is
//! in the `worker` submodule, the watchdog's in `watchdog`, and each worker's own state in
//! `slot`.

use fixedbitset::FixedBitSet;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use crate::backend::{Backend, Raw};
use crate::classify::{ClassOpts, Listing, TaskClass};
use crate::gate::{Gate, GateStats};
use crate::proto::{Batch, DirId, JobId, ListingStatus, ScanMsg};
use crate::sync::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Injector, Mutex, RwLock};

mod slot;
mod watchdog;
mod worker;

#[cfg(test)]
mod test_util;

pub use slot::{Outbox, WorkerSlot};
pub use watchdog::WatchdogReport;
pub use worker::{StepResult, Worker};

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

/// At most this many workers are added to the pool for limited mounts, all mounts together.
pub const MAX_MOUNT_WORKERS: usize = 16;

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
    // resume-keep
    /// Directories not read but reported as [`SkipReason::Kept`](crate::SkipReason::Kept): a
    /// resume's, which keeps them from the tree it loaded.
    pub keep: Option<Arc<crate::journal::Keep>>,
}

pub struct Task<L> {
    pub job: JobId,
    pub dir: DirId,
    pub loc: L,
    focus_epoch: u32,
    is_root: bool,
    /// The directory's own `dev` and mount id (the root's are filled in when it is opened).
    dev: u64,
    mount: u64,
    /// Ask the backend for a subvolume record when opening (`dev` changed from the parent).
    probe_subvol: bool,
    /// Set for a chunk task: one block of the directory to stat. Otherwise this is the
    /// directory's listing task.
    chunk: Option<Box<ChunkWork<L>>>,
    /// The limit of the directory's mount, if it has one. Set for a job root once it is opened;
    /// the root task itself runs without a permit.
    gate: Option<Arc<TaskGate<L>>>,
    /// What the directory's mount is stored on, as the backend reported it when the scan
    /// entered the mount; handed to [`Backend::open`]. A directory inherits its parent's.
    storage: dfdu_sys::Storage,
    /// What the directory's entries are classified against, when the job classifies.
    class: Option<Box<TaskClass>>,
}

/// A mount's concurrency limit, holding the tasks that wait for a permit.
pub type TaskGate<L> = Gate<Task<L>>;

/// Storage class and limit of each mount the scan entered, by mount key; the limit is `None`
/// for a mount without one.
type Gates<L> = HashMap<(u64, u64), (dfdu_sys::Storage, Option<Arc<TaskGate<L>>>)>;

impl<L: std::fmt::Debug> std::fmt::Debug for Task<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("job", &self.job)
            .field("dir", &self.dir)
            .field("loc", &self.loc)
            .field("chunk", &self.chunk.as_ref().map(|c| c.seq))
            .finish_non_exhaustive()
    }
}

/// A block handed from a directory's listing task to a chunk task.
pub struct ChunkWork<L> {
    raw: Raw<L>,
    seq: u32,
    parts: Arc<DirParts>,
    class: Option<Arc<Listing>>,
}

/// The parts of a directory read by more than one task: its listing task and the chunk tasks it
/// handed blocks to. Whoever finishes the last part sends the directory's one `Close`, with
/// every part's outcome in its status.
#[derive(Default)]
pub struct DirParts {
    /// The listing task (until it finishes) plus chunk tasks not finished.
    unfinished: AtomicU32,
    /// Chunk messages of the directory, all parts; set by the listing task when it finishes.
    chunks: AtomicU32,
    panicked: AtomicU32,
    cancelled: AtomicU32,
    /// The listing task's own outcome, set when it finishes.
    listing: Mutex<Option<ListingStatus>>,
}

impl DirParts {
    fn new() -> Self {
        DirParts {
            unfinished: AtomicU32::new(1),
            ..Default::default()
        }
    }

    /// Chunk tasks handed out and not finished.
    fn in_flight(&self) -> usize {
        self.unfinished.load(SeqCst).saturating_sub(1) as usize
    }

    /// The directory's status once every part has finished.
    fn status(&self) -> ListingStatus {
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
const LISTING_PART: u32 = 0;

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
    /// Limits by mount key ([`mount_key`]); `None` for mounts without one. Looked up when a
    /// task for a mount root is created.
    gates: Mutex<Gates<B::Loc>>,
    /// Workers the pool adds for limited mounts other than job roots'.
    mount_workers: AtomicUsize,
}

/// A mount's key in [`Shared::gates`]: its id, or its device when there are no mount ids.
fn mount_key(mount: u64, dev: u64) -> (u64, u64) {
    if mount != 0 { (mount, 0) } else { (0, dev) }
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

    /// The storage class and the limit of the mount `(mount, dev)`, created when the scan first
    /// enters the mount: `Limits::rotational_parts` on a spinning disk, `Limits::network_parts`
    /// on a network mount, none on other storage. The mount of a job root gets the pool's size
    /// instead; any other limited mount adds its limit to the pool, so its tasks do not take
    /// workers from the rest of the scan. The backend answers from the mount table and sysfs,
    /// without touching the mount.
    fn gate_for(
        &self,
        mount: u64,
        dev: u64,
        job_root: bool,
    ) -> (dfdu_sys::Storage, Option<Arc<TaskGate<B::Loc>>>) {
        let key = mount_key(mount, dev);
        let mut gates = self.gates.lock();
        if let Some(g) = gates.get(&key) {
            return g.clone();
        }
        let storage = self.backend.mount_storage(mount, dev);
        let class_limit = match storage {
            dfdu_sys::Storage::Rotational => self.limits.rotational_parts,
            dfdu_sys::Storage::Network => self.limits.network_parts,
            dfdu_sys::Storage::Solid | dfdu_sys::Storage::Unknown => 0,
        };
        let limit = match class_limit {
            0 => 0,
            _ if job_root && self.limits.pool_workers > 0 => self.limits.pool_workers,
            l => l,
        };
        let gate = (limit > 0).then(|| Arc::new(Gate::new(key.0, key.1, limit)));
        if gate.is_some() && !job_root {
            let extra = self.mount_workers.load(SeqCst);
            self.mount_workers
                .store((extra + limit).min(MAX_MOUNT_WORKERS), SeqCst);
        }
        gates.insert(key, (storage, gate.clone()));
        (storage, gate)
    }

    /// Workers the pool adds for limited mounts the scans entered.
    pub fn mount_workers(&self) -> usize {
        self.mount_workers.load(SeqCst)
    }

    /// Every mount limit created so far.
    pub fn gate_stats(&self) -> Vec<GateStats> {
        let gates: Vec<_> = self
            .gates
            .lock()
            .values()
            .filter_map(|g| g.1.clone())
            .collect();
        gates.iter().map(|g| g.stats()).collect()
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

    fn job(&self, job: JobId) -> Option<Arc<JobState>> {
        self.jobs.read().get(&job.0).cloned()
    }

    /// Submits the root task of a job. The applier creates the root placeholder first. Returns
    /// the `submitted_seq` value that covers this job.
    pub fn start_job(&self, job: JobId, root: &Path, opts: JobOptions) -> u64 {
        let st = Arc::new(JobState {
            ended: AtomicBool::new(false),
            next_dir: AtomicU32::new(1),
            early_stop: RwLock::new(FixedBitSet::new()),
            dup_mounts: (opts.cross_mounts || opts.network)
                .then(|| {
                    self.backend
                        .duplicate_mounts(opts.scope.as_deref().unwrap_or(root))
                })
                .flatten(),
            mount_roots: Mutex::new(HashSet::new()),
            opts,
        });
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
