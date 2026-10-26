//! Scanner state shared by workers, the watchdog and the applier, and the worker and watchdog
//! step functions (`docs/data-pipeline.md` §3, §4.3, §5.4, §5.7).

use fixedbitset::FixedBitSet;
use std::collections::{HashMap, HashSet};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use dut_tree::{ErrKind, Kind};

use crate::FsClass;
use crate::backend::{Backend, Block, DirStream, Raw};
use crate::proto::{
    Batch, Chunk, ChunkStatus, DirId, EntryError, FileRec, JobId, ListingStatus, Probe, ScanMsg,
    SkipReason, SubdirRec,
};
use crate::sync::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Injector, Mutex, RwLock, Steal};

/// Monotonic time in nanoseconds; never returns 0 (0 means "not set" in the slot fields).
pub trait Clock: Sync {
    fn now(&self) -> u64;
}

/// Where flushed batches go: the applier's inbox.
pub trait Sink: Sync {
    /// Sends without blocking; returns the batch if the byte budget is exhausted.
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
    /// Chunk tasks a directory may have outstanding (§4.3); 0 never hands a block to a chunk
    /// task. The engine sets it to `max(2, workers)` when it is 0.
    pub chunk_credit: usize,
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
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct JobOptions {
    /// Descend into directories on other filesystems.
    pub cross_mounts: bool,
    /// Descend into network filesystems (NFS, SMB, sshfs and the like).
    pub network: bool,
    /// The directory whose mounts are checked for duplicates: the visible scan root when a
    /// rescan covers only part of it. Defaults to the job root.
    pub scope: Option<PathBuf>,
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
    /// Set for a chunk task: one block of the directory to stat (§4.2). Otherwise this is the
    /// directory's listing task.
    chunk: Option<Box<ChunkWork<L>>>,
}

impl<L: std::fmt::Debug> std::fmt::Debug for Task<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("job", &self.job)
            .field("dir", &self.dir)
            .field("loc", &self.loc)
            .field("chunk", &self.chunk.as_ref().map(|c| c.seq))
            .finish()
    }
}

/// A block handed from a directory's listing task to a chunk task.
pub struct ChunkWork<L> {
    raw: Raw<L>,
    seq: u32,
    parts: Arc<DirParts>,
}

/// The parts of a directory read by more than one task: its listing task and the chunk tasks
/// it handed blocks to. Whoever finishes the last part sends the directory's one `Close`
/// (§4.2), with every part's outcome in its status.
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
    /// With `cross_mounts`: mount ids not to descend into (`docs/btrfs.md` §3.2), from the
    /// mount table at job start.
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
    ((job.0 as u64) << 32) | dir.0 as u64
}

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
    pub waiting_budget: AtomicBool,
}

impl<L> WorkerSlot<L> {
    fn new() -> Self {
        WorkerSlot {
            outbox: Mutex::new(Outbox::default()),
            syscall_since: AtomicU64::new(0),
            current: AtomicU64::new(NO_TASK),
            part: AtomicU32::new(LISTING_PART),
            stall_reported: AtomicBool::new(false),
            waiting_budget: AtomicBool::new(false),
        }
    }

    pub fn in_syscall_since(&self) -> u64 {
        self.syscall_since.load(SeqCst)
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

    fn job(&self, job: JobId) -> Option<Arc<JobState>> {
        self.jobs.read().get(&job.0).cloned()
    }

    // ---- applier API ---------------------------------------------------------------------

    /// Submits the root task of a job. The applier creates the root placeholder first.
    /// Returns the `submitted_seq` value that covers this job (§5.7).
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

    /// Makes `(job, dir)` the focus (§4.3). The focus is stored before the epoch changes;
    /// readers load the epoch first (`Worker::emit_block`).
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
        for (i, slot) in self.slots.read().iter().enumerate() {
            let cur = slot.current.load(SeqCst);
            let ob = slot.outbox.lock();
            out += &format!(
                "slot {i}: current {:?} part {} since {} tasks {:?} done {:?} msgs {}\n",
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

    // ---- flushing ------------------------------------------------------------------------

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

    // ---- watchdog --------------------------------------------------------------------------

    /// One watchdog round (§3 rule 6, §5.4, §5.7). Never blocks.
    pub fn watchdog_round(&self, sink: &dyn Sink, now: u64) -> WatchdogReport {
        let mut rep = WatchdogReport::default();
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
            let old =
                !ob.msgs.is_empty() && now.saturating_sub(ob.oldest) > self.limits.flush_age_ns;
            if force || old || (ob.msgs.is_empty() && !ob.is_empty()) {
                self.flush_locked(&mut ob, sink, false);
            }
        }
        let seq = self.submitted_seq.load(SeqCst);
        let (out, ab) = self.outstanding();
        if out == ab && self.last_quiescent.swap(seq, SeqCst) != seq {
            sink.send_control(ScanMsg::Quiescent { submitted_seq: seq });
        }
        rep
    }

    /// How many workers beyond the pool's target may exist (§5.4): one per stuck worker, at
    /// most `min(target, 16)` for stalls still being waited on, and at most 64 more for workers
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StepResult {
    /// Did some work.
    Progress,
    /// No task available; the outbox has been flushed (or could not be, in non-blocking mode).
    Idle,
    /// The backend reported "not ready" (simulation only).
    Blocked,
    /// A required flush could not get budget (non-blocking mode only).
    WaitingBudget,
}

struct Running<B: Backend> {
    task: Task<B::Loc>,
    job: Arc<JobState>,
    stream: Option<B::Stream>,
    /// A block this listing task is stat'ing itself: its seq and whether more blocks follow.
    /// Kept across "not ready" returns, and set while stat'ing so a panic there costs only
    /// this block.
    inline: Option<(Raw<B::Loc>, u32, bool)>,
    seq: u32,
    /// Entries read so far; a read error before any entry means the directory failed.
    entries: usize,
    panicked: u32,
    read_error: Option<ErrKind>,
    /// Set once the listing hands a block to a chunk task.
    parts: Option<Arc<DirParts>>,
}

/// One worker's private state. `blocking` selects real-thread behaviour (flushes wait for
/// budget) or simulation behaviour (flushes that cannot proceed are retried later).
pub struct Worker<B: Backend> {
    pub slot: Arc<WorkerSlot<B::Loc>>,
    running: Option<Running<B>>,
    block: Block<B::Loc>,
    blocking: bool,
}

impl<B: Backend> Worker<B> {
    pub fn new(shared: &Shared<B>, blocking: bool) -> Self {
        Worker {
            slot: shared.new_slot(),
            running: None,
            block: Block::default(),
            blocking,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Runs one step: take a task, open a directory, read one block, or stat one. Panics are
    /// caught and recorded against the directory (§4.2).
    pub fn step(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        if self.must_flush(sh) && !self.flush(sh, sink) {
            return StepResult::WaitingBudget;
        }
        let r = catch_unwind(AssertUnwindSafe(|| self.step_inner(sh, sink, clock)));
        let result = match r {
            Ok(r) => r,
            Err(payload) => {
                self.slot.syscall_since.store(0, SeqCst);
                self.on_panic(panic_message(&*payload), clock);
                StepResult::Progress
            }
        };
        if result == StepResult::Progress && self.must_flush(sh) && !self.flush(sh, sink) {
            return StepResult::WaitingBudget;
        }
        result
    }

    /// Records a caught panic: the block being stat'ed is lost (the listing goes on), or the
    /// listing ends there.
    fn on_panic(&mut self, msg: String, clock: &dyn Clock) {
        self.block.clear();
        let Some(mut r) = self.running.take() else {
            return;
        };
        let now = clock.now();
        if let Some(work) = r.task.chunk.take() {
            let chunk = empty_chunk(&r.task, work.seq, ChunkStatus::Panicked(msg));
            work.parts.panicked.fetch_add(1, SeqCst);
            let mut ob = self.slot.outbox.lock();
            ob.push(ScanMsg::Chunk(chunk), now);
            self.finish_part(&mut ob, &r.task, &work.parts, work.seq + 1, now);
            return;
        }
        if let Some((_, seq, more)) = r.inline.take() {
            let chunk = empty_chunk(&r.task, seq, ChunkStatus::Panicked(msg));
            r.panicked += 1;
            self.slot.outbox.lock().push(ScanMsg::Chunk(chunk), now);
            let status = Self::ended_status(&r);
            self.running = Some(r);
            if !more {
                self.finish_listing(status, clock);
            }
            return;
        }
        // Opening or reading failed: the listing ends with what was read.
        let chunk = empty_chunk(&r.task, r.seq, ChunkStatus::Panicked(msg));
        r.seq += 1;
        r.panicked += 1;
        self.slot.outbox.lock().push(ScanMsg::Chunk(chunk), now);
        let status = Self::ended_status(&r);
        self.running = Some(r);
        self.finish_listing(status, clock);
    }

    /// The listing's status when it ends after reading what it could.
    fn ended_status(r: &Running<B>) -> ListingStatus {
        match (r.panicked, r.read_error) {
            (0, None) => ListingStatus::Complete,
            (p, e) => ListingStatus::Partial {
                panicked: p,
                cancelled: 0,
                read_error: e,
            },
        }
    }

    fn must_flush(&self, sh: &Shared<B>) -> bool {
        let ob = self.slot.outbox.lock();
        ob.bytes >= sh.limits.outbox_bytes
            || ob.msgs.len() >= sh.limits.outbox_msgs
            || (sh.idle.load(SeqCst) > 0 && (ob.tasks.len() > 1 || ob.has_chunk_task))
    }

    /// Flushes the outbox. In blocking mode, waits for budget.
    pub fn flush(&mut self, sh: &Shared<B>, sink: &dyn Sink) -> bool {
        let mut ob = self.slot.outbox.lock();
        if ob.is_empty() {
            return true;
        }
        self.slot.waiting_budget.store(true, SeqCst);
        let ok = sh.flush_locked(&mut ob, sink, self.blocking);
        self.slot.waiting_budget.store(false, SeqCst);
        ok
    }

    /// Ends the current part: it no longer counts as running, and the next flush reports it
    /// done.
    fn end_part(&self, ob: &mut Outbox<B::Loc>, task: &Task<B::Loc>, part: u32) {
        ob.done.push((task.job, task.dir, part));
        self.slot.current.store(NO_TASK, SeqCst);
        self.slot.stall_reported.store(false, SeqCst);
        // A part can end while its last call reported "not ready" (stopped after a stall); the
        // next task must not inherit that call's start time and look stalled.
        self.slot.syscall_since.store(0, SeqCst);
    }

    /// Ends one part of a directory that has several; the last part sends the `Close`.
    fn finish_part(
        &self,
        ob: &mut Outbox<B::Loc>,
        task: &Task<B::Loc>,
        parts: &DirParts,
        part: u32,
        now: u64,
    ) {
        if parts.unfinished.fetch_sub(1, SeqCst) == 1 {
            ob.push(
                ScanMsg::Close {
                    job: task.job,
                    dir: task.dir,
                    chunks: parts.chunks.load(SeqCst),
                    status: parts.status(),
                },
                now,
            );
        }
        self.end_part(ob, task, part);
    }

    /// Ends the listing task with `status`: sends the `Close`, or leaves it to the last chunk
    /// task still running.
    fn finish_listing(&mut self, status: ListingStatus, clock: &dyn Clock) {
        let r = self.running.take().expect("running task");
        let mut ob = self.slot.outbox.lock();
        let now = clock.now();
        match &r.parts {
            None => {
                ob.push(
                    ScanMsg::Close {
                        job: r.task.job,
                        dir: r.task.dir,
                        chunks: r.seq,
                        status,
                    },
                    now,
                );
                self.end_part(&mut ob, &r.task, LISTING_PART);
            }
            Some(parts) => {
                *parts.listing.lock() = Some(status);
                parts.chunks.store(r.seq, SeqCst);
                self.finish_part(&mut ob, &r.task, parts, LISTING_PART, now);
            }
        }
    }

    fn take_task(&self, sh: &Shared<B>) -> Option<Task<B::Loc>> {
        let set = |t: &Task<B::Loc>| {
            self.slot.current.store(pack(t.job, t.dir), SeqCst);
            let part = t.chunk.as_ref().map_or(LISTING_PART, |c| c.seq + 1);
            self.slot.part.store(part, SeqCst);
        };
        {
            let mut ob = self.slot.outbox.lock();
            if let Some(t) = ob.tasks.pop() {
                set(&t);
                return Some(t);
            }
        }
        let t = steal(&sh.priority).or_else(|| steal(&sh.injector))?;
        let _ob = self.slot.outbox.lock();
        set(&t);
        Some(t)
    }

    fn step_inner(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        let Some(r) = self.running.as_mut() else {
            let Some(task) = self.take_task(sh) else {
                let mut ob = self.slot.outbox.lock();
                if !ob.is_empty() {
                    sh.flush_locked(&mut ob, sink, self.blocking);
                }
                return StepResult::Idle;
            };
            let job = sh.job(task.job);
            let live = job
                .as_ref()
                .is_some_and(|j| !j.ended.load(SeqCst) && !j.stopped(task.dir));
            match (job, live) {
                (Some(job), true) => {
                    self.running = Some(Running {
                        task,
                        job,
                        stream: None,
                        inline: None,
                        seq: 0,
                        entries: 0,
                        panicked: 0,
                        read_error: None,
                        parts: None,
                    });
                }
                _ => {
                    let mut ob = self.slot.outbox.lock();
                    let now = clock.now();
                    match &task.chunk {
                        // Still one chunk message, so the directory's count is right.
                        Some(work) => {
                            let chunk = empty_chunk(&task, work.seq, ChunkStatus::Cancelled);
                            ob.push(ScanMsg::Chunk(chunk), now);
                            work.parts.cancelled.fetch_add(1, SeqCst);
                            self.finish_part(&mut ob, &task, &work.parts, work.seq + 1, now);
                        }
                        None => {
                            ob.push(
                                ScanMsg::Close {
                                    job: task.job,
                                    dir: task.dir,
                                    chunks: 0,
                                    status: ListingStatus::Cancelled,
                                },
                                now,
                            );
                            self.end_part(&mut ob, &task, LISTING_PART);
                        }
                    }
                }
            }
            return StepResult::Progress;
        };

        if r.task.chunk.is_some() {
            return self.chunk_step(sh, clock);
        }

        if r.stream.is_none() {
            self.slot.begin_syscall(clock.now());
            let opened = if r.task.is_root {
                sh.backend.stat_root(&r.task.loc).and_then(|m| {
                    r.task.dev = m.dev;
                    r.task.mount = m.mount;
                    r.job.mount_roots.lock().insert((m.dev, m.ino));
                    sh.backend.open(&r.task.loc).map(|s| (s, Some(m)))
                })
            } else {
                sh.backend.open(&r.task.loc).map(|s| (s, None))
            };
            // Part of opening, under the same stall watch. A failed probe only loses the record.
            let probe = match &opened {
                Ok(_) if r.task.probe_subvol => Some(Probe {
                    subvol: sh.backend.subvolume(&r.task.loc).ok().flatten(),
                    shares_extents: sh.backend.shares_extents(&r.task.loc),
                    profile: if r.task.is_root {
                        sh.backend.data_profile(&r.task.loc)
                    } else {
                        None
                    },
                }),
                _ => None,
            };
            self.slot.syscall_since.store(0, SeqCst);
            match opened {
                Ok((stream, root_meta)) => {
                    r.stream = Some(stream);
                    if root_meta.is_some() || probe.is_some() {
                        // The root's own stat and the probe travel in the first chunk.
                        let mut c = empty_chunk(&r.task, r.seq, ChunkStatus::Ok);
                        c.root_meta = root_meta;
                        c.probe = probe.map(Box::new);
                        r.seq += 1;
                        self.slot.outbox.lock().push(ScanMsg::Chunk(c), clock.now());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.slot.begin_syscall(clock.now());
                    return StepResult::Blocked;
                }
                Err(e) => self.finish_listing(ListingStatus::Failed(ErrKind::from(&e)), clock),
            }
            return StepResult::Progress;
        }

        // A block being stat'ed is finished first, like a blocking call that has begun.
        if r.inline.is_some() {
            return self.inline_step(sh, clock);
        }

        if r.job.ended.load(SeqCst) || r.job.stopped(r.task.dir) {
            let status = if r.entries == 0 {
                ListingStatus::Cancelled
            } else {
                ListingStatus::Partial {
                    panicked: r.panicked,
                    cancelled: 1,
                    read_error: None,
                }
            };
            self.block.clear();
            self.finish_listing(status, clock);
            return StepResult::Progress;
        }

        let limits = &sh.limits;
        self.slot.begin_syscall(clock.now());
        let res = r
            .stream
            .as_mut()
            .expect("open stream")
            .next_raw(limits.block_entries, limits.block_name_bytes);
        if !matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.slot.syscall_since.store(0, SeqCst);
        }
        let (raw, more) = match res {
            Ok(x) => x,
            // The call is still in progress as far as the watchdog is concerned.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return StepResult::Blocked,
            Err(e) => {
                let err = ErrKind::from(&e);
                if r.entries == 0 {
                    self.finish_listing(ListingStatus::Failed(err), clock);
                    return StepResult::Progress;
                }
                r.read_error = Some(err);
                let status = Self::ended_status(r);
                self.finish_listing(status, clock);
                return StepResult::Progress;
            }
        };
        r.entries += raw.len();
        if raw.is_empty() {
            if !more {
                let status = Self::ended_status(r);
                self.finish_listing(status, clock);
            }
            return StepResult::Progress;
        }
        let seq = r.seq;
        r.seq += 1;
        let credit = limits.chunk_credit;
        let free = r
            .parts
            .as_ref()
            .map_or(credit, |p| credit - p.in_flight().min(credit));
        if more && free > 0 {
            // More blocks follow: another worker can stat this one (§4.2).
            let parts = r
                .parts
                .get_or_insert_with(|| Arc::new(DirParts::new()))
                .clone();
            parts.unfinished.fetch_add(1, SeqCst);
            let task = Task {
                job: r.task.job,
                dir: r.task.dir,
                loc: r.task.loc.clone(),
                focus_epoch: r.task.focus_epoch,
                is_root: false,
                dev: r.task.dev,
                mount: r.task.mount,
                probe_subvol: false,
                chunk: Some(Box::new(ChunkWork { raw, seq, parts })),
            };
            let mut ob = self.slot.outbox.lock();
            sh.outstanding.fetch_add(1, SeqCst);
            sh.submitted_seq.fetch_add(1, SeqCst);
            sh.chunk_tasks.fetch_add(1, SeqCst);
            ob.tasks.push(task);
            ob.has_chunk_task = true;
            return StepResult::Progress;
        }
        r.inline = Some((raw, seq, more));
        self.inline_step(sh, clock)
    }

    /// The listing task stats a block itself.
    fn inline_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        let (raw, _, _) = r.inline.as_mut().expect("inline block");
        self.slot.begin_syscall(clock.now());
        let res = raw.stat(&mut self.block);
        if matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return StepResult::Blocked;
        }
        self.slot.syscall_since.store(0, SeqCst);
        let (_, seq, more) = r.inline.take().expect("inline block");
        if let Err(e) = res {
            // Only "not ready" is expected; anything else loses the block, as a panic would.
            let chunk = empty_chunk(&r.task, seq, ChunkStatus::Panicked(e.to_string()));
            r.panicked += 1;
            self.block.clear();
            self.slot
                .outbox
                .lock()
                .push(ScanMsg::Chunk(chunk), clock.now());
        } else {
            self.emit_block(sh, seq, clock);
        }
        if !more {
            let status = Self::ended_status(self.running.as_ref().expect("running"));
            self.finish_listing(status, clock);
        }
        StepResult::Progress
    }

    /// A chunk task: stat its block and send it as one chunk.
    fn chunk_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        // Stopped chunk tasks were turned away when taken; once stat'ing has begun it runs to
        // the end, like a blocking call.
        let work = r.task.chunk.as_mut().expect("chunk task");
        self.slot.begin_syscall(clock.now());
        let res = work.raw.stat(&mut self.block);
        if matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return StepResult::Blocked;
        }
        self.slot.syscall_since.store(0, SeqCst);
        let seq = work.seq;
        match res {
            Ok(()) => self.emit_block(sh, seq, clock),
            Err(e) => {
                self.block.clear();
                let r = self.running.as_mut().expect("running");
                let work = r.task.chunk.as_ref().expect("chunk task");
                work.parts.panicked.fetch_add(1, SeqCst);
                let chunk = empty_chunk(&r.task, seq, ChunkStatus::Panicked(e.to_string()));
                self.slot
                    .outbox
                    .lock()
                    .push(ScanMsg::Chunk(chunk), clock.now());
            }
        }
        let mut r = self.running.take().expect("running");
        let work = r.task.chunk.take().expect("chunk task");
        let mut ob = self.slot.outbox.lock();
        self.finish_part(&mut ob, &r.task, &work.parts, seq + 1, clock.now());
        StepResult::Progress
    }

    /// Turns the stat'ed block into chunk `seq`, creating tasks for subdirectories. Tasks go
    /// on the outbox's private stack; they become stealable when the outbox is flushed.
    fn emit_block(&mut self, sh: &Shared<B>, seq: u32, clock: &dyn Clock) {
        let r = self.running.as_mut().expect("running");
        let block = std::mem::take(&mut self.block);
        let (cross, network) = (r.job.opts.cross_mounts, r.job.opts.network);
        let (dir_dev, dir_mount) = (r.task.dev, r.task.mount);
        // Epoch first: `set_focus` stores the focus before it bumps the epoch, so a new epoch
        // read here comes with the new focus. The other order could pair the old focus with
        // the new epoch and keep the old focus's children prioritised after a change.
        let epoch = sh.focus_epoch.load(SeqCst);
        let focus = sh.focus.load(SeqCst);
        let child_epoch = if focus == pack(r.task.job, r.task.dir) || r.task.focus_epoch == epoch {
            epoch
        } else {
            0
        };

        let mut files = Vec::new();
        let mut subdirs = Vec::new();
        let mut errors = Vec::new();
        let mut child_locs = Vec::new();
        for e in block.entries {
            match e.meta {
                Err(err) => errors.push(EntryError { name: e.name, err }),
                Ok(m) if m.kind == Kind::Dir => {
                    // Mount ids when both are known; otherwise a change of `dev` (which also
                    // happens at btrfs subvolumes, so they are only traversed with mount ids).
                    let mount_root = if m.mount != 0 && dir_mount != 0 {
                        m.mount != dir_mount
                    } else {
                        m.dev != dir_dev
                    };
                    // Kernel filesystems are never scanned; network ones only when asked for,
                    // decided from the mount table without touching the mount.
                    let class = if mount_root {
                        sh.backend.mount_class(m.mount, m.dev)
                    } else {
                        None
                    };
                    let wanted = match class {
                        Some(FsClass::Kernel) => Err(SkipReason::Kernel),
                        Some(FsClass::Network) if network => Ok(()),
                        Some(FsClass::Network) => Err(SkipReason::Network),
                        _ if cross => Ok(()),
                        _ => Err(SkipReason::OtherFilesystem),
                    };
                    let skip = if !mount_root {
                        None
                    } else if let Err(why) = wanted {
                        Some(why)
                    } else if match &r.job.dup_mounts {
                        Some(d) => d.contains(&m.mount),
                        None => !r.job.mount_roots.lock().insert((m.dev, m.ino)),
                    } {
                        Some(SkipReason::DuplicateMount)
                    } else {
                        None
                    };
                    let loc = if skip.is_none() { e.child } else { None };
                    let skip = if skip.is_none() && loc.is_none() {
                        // A directory the backend gave no location for cannot be read.
                        Some(SkipReason::Excluded)
                    } else {
                        skip
                    };
                    child_locs.push(loc.map(|l| (l, m.dev, m.mount, m.dev != dir_dev)));
                    subdirs.push(SubdirRec {
                        name: e.name,
                        meta: m,
                        mount_root,
                        skip,
                    });
                }
                Ok(m) => files.push(FileRec {
                    name: e.name,
                    meta: m,
                }),
            }
        }
        let first = r.job.next_dir.fetch_add(subdirs.len() as u32, SeqCst);
        let mut tasks = Vec::new();
        for (i, loc) in child_locs.into_iter().enumerate() {
            if let Some((loc, dev, mount, probe_subvol)) = loc {
                tasks.push(Task {
                    job: r.task.job,
                    dir: DirId(first + i as u32),
                    loc,
                    focus_epoch: child_epoch,
                    is_root: false,
                    dev,
                    mount,
                    probe_subvol,
                    chunk: None,
                });
            }
        }
        let chunk = Chunk {
            job: r.task.job,
            dir: r.task.dir,
            seq,
            root_meta: None,
            probe: None,
            names: block.names,
            files,
            first_subdir: DirId(first),
            subdirs,
            errors,
            status: ChunkStatus::Ok,
        };
        let mut ob = self.slot.outbox.lock();
        // Counted only once nothing fallible remains, so a panic above cannot leave tasks
        // counted as outstanding that will never close.
        sh.outstanding.fetch_add(tasks.len() as u64, SeqCst);
        sh.submitted_seq.fetch_add(tasks.len() as u64, SeqCst);
        ob.push(ScanMsg::Chunk(chunk), clock.now());
        // Reverse so the first subdirectory is popped first (depth-first, in read order).
        ob.tasks.extend(tasks.into_iter().rev());
    }
}

fn empty_chunk<L>(task: &Task<L>, seq: u32, status: ChunkStatus) -> Chunk {
    Chunk {
        job: task.job,
        dir: task.dir,
        seq,
        root_meta: None,
        probe: None,
        names: Vec::new(),
        files: Vec::new(),
        first_subdir: DirId(0),
        subdirs: Vec::new(),
        errors: Vec::new(),
        status,
    }
}

fn steal<T>(inj: &Injector<T>) -> Option<T> {
    loop {
        match inj.steal() {
            Steal::Success(t) => return Some(t),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

/// The message of a caught panic payload (`panic!` with a literal or a formatted string).
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}
