//! Scanner state shared by workers, the watchdog and the applier, and the watchdog step
//! function. The worker step function is in the `worker` submodule.

use fixedbitset::FixedBitSet;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use crate::backend::{Backend, Raw};
use crate::classify::{ClassOpts, Listing, TaskClass};
use crate::gate::{Gate, GateStats};
use crate::proto::{Batch, DirId, JobId, ListingStatus, ScanMsg};
use crate::sync::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Injector, Mutex, RwLock};

mod worker;

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

/// Unsent messages and unpublished tasks of one worker. Held under the slot's mutex only to
/// append, pop or flush; never across a filesystem call.
pub struct Outbox<L> {
    msgs: Vec<ScanMsg>,
    bytes: usize,
    oldest: u64,
    /// Parts (tasks) finished since the last flush: `(job, dir, part)`.
    done: Vec<(JobId, DirId, u32)>,
    tasks: Vec<Task<L>>,
    /// A chunk task is among `tasks`: publish it soon, others may be idle.
    has_chunk_task: bool,
    /// An own task could not run for its mount's limit: publish it, so it can wait in the
    /// mount's gate and the tasks under it can be taken.
    gated_task: bool,
    /// The gate whose permit the running part holds.
    permit: Option<Arc<TaskGate<L>>>,
    /// A task handed over with a permit by the part that released it; runs next.
    handoff: Option<Task<L>>,
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
    fn push(&mut self, msg: ScanMsg, now: u64) {
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

/// Per-worker state visible to the watchdog and the applier.
pub struct WorkerSlot<L> {
    pub outbox: Mutex<Outbox<L>>,
    /// Time the worker entered its current filesystem call, or 0.
    syscall_since: AtomicU64,
    /// Task being run, packed `(job, dir)`, or `NO_TASK`, and which part of the directory it
    /// is. Changed only under the outbox lock.
    current: AtomicU64,
    part: AtomicU32,
    stall_reported: AtomicBool,
}

impl<L> WorkerSlot<L> {
    fn new() -> Self {
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
    fn begin_syscall(&self, now: u64) {
        let _ = self.syscall_since.compare_exchange(0, now, SeqCst, SeqCst);
    }

    /// Marks the end of a filesystem call, or of whatever call the part left open.
    fn end_syscall(&self) {
        self.syscall_since.store(0, SeqCst);
    }

    /// Runs a filesystem call under the stall watch. A call that reports "not ready"
    /// (`WouldBlock`) stays open, so its retry keeps the original start time.
    fn watch_io<T>(&self, now: u64, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.begin_syscall(now);
        let res = call();
        if !matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.end_syscall();
        }
        res
    }

    /// Runs a filesystem call that cannot report "not ready" under the stall watch.
    fn watch<T>(&self, now: u64, call: impl FnOnce() -> T) -> T {
        self.begin_syscall(now);
        let res = call();
        self.end_syscall();
        res
    }
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

    /// One watchdog round. Never blocks.
    pub fn watchdog_round(&self, sink: &dyn Sink, now: u64) -> WatchdogReport {
        let mut rep = WatchdogReport::default();
        let mut requeue = Vec::new();
        for slot in self.slots.read().iter() {
            let since = slot.syscall_since.load(SeqCst);
            let is_stuck = since != 0 && now.saturating_sub(since) > self.limits.stall_ns;
            if is_stuck {
                rep.stuck += 1;
                let cur = slot.current.load(SeqCst);
                let (job, dir) = unpack(cur);
                if cur != NO_TASK
                    && self
                        .abandoned
                        .lock()
                        .contains(&(job, dir, slot.part.load(SeqCst)))
                {
                    rep.stuck_abandoned += 1;
                }
            }
            let Some(mut ob) = slot.outbox.try_lock() else {
                continue;
            };
            let mut force = false;
            // Looked at again under the lock: the call seen above may have returned and the
            // worker moved on to another part, which must not be reported as stalled. A part
            // ends (clearing the start time) and the next one is taken under this lock, so a
            // start time read here belongs to the part in `current`.
            let since = slot.syscall_since.load(SeqCst);
            let is_stuck = since != 0 && now.saturating_sub(since) > self.limits.stall_ns;
            if is_stuck && !slot.stall_reported.load(SeqCst) {
                let cur = slot.current.load(SeqCst);
                if cur != NO_TASK {
                    let (job, dir) = unpack(cur);
                    ob.push(ScanMsg::Stalled { job, dir }, now);
                    slot.stall_reported.store(true, SeqCst);
                    force = true;
                }
            }
            // A part stuck in a call does not count against its mount's limit: its permit goes
            // back, and a task waiting for one is queued again.
            if is_stuck
                && let Some(g) = &ob.permit
                && let Some(next) = g.revoke()
            {
                ob.permit = None;
                requeue.extend(next);
            }
            let old =
                !ob.msgs.is_empty() && now.saturating_sub(ob.oldest) > self.limits.flush_age_ns;
            if force || old || (ob.msgs.is_empty() && !ob.is_empty()) {
                self.flush_locked(&mut ob, sink, false);
            }
        }
        if !requeue.is_empty() {
            // They were published before they waited, so they go to the queue directly.
            for t in requeue {
                self.injector.push(t);
            }
            self.wake();
        }
        let seq = self.submitted_seq.load(SeqCst);
        let (out, ab) = self.outstanding();
        if out == ab && self.last_quiescent.swap(seq, SeqCst) != seq {
            sink.send_control(ScanMsg::Quiescent { submitted_seq: seq });
        }
        rep
    }

    /// How many workers beyond the pool's target may exist: one per stuck worker, at most
    /// `min(target, 16)` for stalls still being waited on, and at most 64 more for workers
    /// stuck on tasks the applier abandoned. A thread stuck in a call cannot be stopped, so
    /// without the second allowance a few abandoned stalls could leave no worker for the rest.
    pub fn allowed_extra_workers(&self, target: usize, rep: &WatchdogReport) -> usize {
        let live = rep.stuck - rep.stuck_abandoned;
        live.min(target.min(16)) + rep.stuck_abandoned.min(64)
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchdogReport {
    /// Workers inside one filesystem call for longer than the stall limit.
    pub stuck: usize,
    /// Of those, the ones whose task the applier has abandoned.
    pub stuck_abandoned: usize,
}

/// The message of a caught panic payload (`panic!` with a literal or a formatted string).
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(std::string::ToString::to_string)
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}
