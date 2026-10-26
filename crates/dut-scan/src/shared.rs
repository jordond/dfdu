//! Scanner state shared by workers, the watchdog and the applier, and the worker and watchdog
//! step functions (`docs/data-pipeline.md` §3, §4.3, §5.4, §5.7).

use fixedbitset::FixedBitSet;
use std::collections::{HashMap, HashSet};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};

use crossbeam_deque::{Injector, Steal};
use dut_tree::{ErrKind, Kind};
use parking_lot::{Mutex, RwLock};

use crate::FsClass;
use crate::backend::{Backend, Block, DirStream};
use crate::proto::{
    Batch, Chunk, ChunkStatus, DirId, EntryError, FileRec, JobId, ListingStatus, Probe, ScanMsg,
    SkipReason, SubdirRec,
};

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

#[derive(Debug)]
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
}

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
    closes: Vec<(JobId, DirId)>,
    tasks: Vec<Task<L>>,
}

impl<L> Default for Outbox<L> {
    fn default() -> Self {
        Outbox {
            msgs: Vec::new(),
            bytes: 0,
            oldest: 0,
            closes: Vec::new(),
            tasks: Vec::new(),
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
    pub fn is_empty(&self) -> bool {
        self.msgs.is_empty() && self.tasks.is_empty()
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
    /// Task being run, packed `(job, dir)`, or `NO_TASK`. Changed only under the outbox lock.
    current: AtomicU64,
    stall_reported: AtomicBool,
    pub waiting_budget: AtomicBool,
}

impl<L> WorkerSlot<L> {
    fn new() -> Self {
        WorkerSlot {
            outbox: Mutex::new(Outbox::default()),
            syscall_since: AtomicU64::new(0),
            current: AtomicU64::new(NO_TASK),
            stall_reported: AtomicBool::new(false),
            waiting_budget: AtomicBool::new(false),
        }
    }

    pub fn in_syscall_since(&self) -> u64 {
        self.syscall_since.load(SeqCst)
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
    abandoned: Mutex<HashSet<(JobId, DirId)>>,
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

    /// Records that the applier abandoned `(job, dir)`. Returns `false` if it could not take a
    /// worker's outbox lock without blocking; the applier retries on its next slice. If no
    /// worker is still running the task, its `Close` is already on the way and nothing is
    /// recorded.
    pub fn abandon(&self, job: JobId, dir: DirId) -> bool {
        self.stop_dirs(job, [dir]);
        let target = pack(job, dir);
        for slot in self.slots.read().iter() {
            let Some(_ob) = slot.outbox.try_lock() else {
                if slot.current.load(SeqCst) == target {
                    return false;
                }
                continue;
            };
            if slot.current.load(SeqCst) == target {
                self.abandoned.lock().insert((job, dir));
                self.outstanding_abandoned.fetch_add(1, SeqCst);
                return true;
            }
        }
        true
    }

    pub fn set_focus(&self, job: JobId, dir: DirId) {
        self.focus.store(pack(job, dir), SeqCst);
        self.focus_epoch.fetch_add(1, SeqCst);
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
        if !ob.closes.is_empty() {
            let mut ab = self.abandoned.lock();
            for c in ob.closes.drain(..) {
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
                if cur != NO_TASK && self.abandoned.lock().contains(&unpack(cur)) {
                    rep.stuck_abandoned += 1;
                }
            }
            let Some(mut ob) = slot.outbox.try_lock() else {
                continue;
            };
            let mut force = false;
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
            if force || old || (ob.msgs.is_empty() && !ob.tasks.is_empty()) {
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
    seq: u32,
    /// Entries read so far; a read error before any entry means the directory failed.
    entries: usize,
    panicked: u32,
    read_error: Option<ErrKind>,
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

    /// Runs one step: take a task, open a directory, or read one block. Panics are caught and
    /// recorded against the directory being read (§4.2).
    pub fn step(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        if self.must_flush(sh) && !self.flush(sh, sink) {
            return StepResult::WaitingBudget;
        }
        let r = catch_unwind(AssertUnwindSafe(|| self.step_inner(sh, sink, clock)));
        let result = match r {
            Ok(r) => r,
            Err(payload) => {
                self.slot.syscall_since.store(0, SeqCst);
                let msg = panic_message(&*payload);
                self.block.clear();
                if let Some(r) = self.running.take() {
                    let chunk = empty_chunk(&r, ChunkStatus::Panicked(msg));
                    let chunks = r.seq + 1;
                    let mut ob = self.slot.outbox.lock();
                    ob.push(ScanMsg::Chunk(chunk), clock.now());
                    let status = ListingStatus::Partial {
                        panicked: r.panicked + 1,
                        cancelled: 0,
                        read_error: r.read_error,
                    };
                    self.close_locked(&mut ob, &r.task, chunks, status, clock.now());
                }
                StepResult::Progress
            }
        };
        if result == StepResult::Progress && self.must_flush(sh) && !self.flush(sh, sink) {
            return StepResult::WaitingBudget;
        }
        result
    }

    fn must_flush(&self, sh: &Shared<B>) -> bool {
        let ob = self.slot.outbox.lock();
        ob.bytes >= sh.limits.outbox_bytes
            || ob.msgs.len() >= sh.limits.outbox_msgs
            || (sh.idle.load(SeqCst) > 0 && ob.tasks.len() > 1)
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

    fn close_locked(
        &self,
        ob: &mut Outbox<B::Loc>,
        task: &Task<B::Loc>,
        chunks: u32,
        status: ListingStatus,
        now: u64,
    ) {
        ob.push(
            ScanMsg::Close {
                job: task.job,
                dir: task.dir,
                chunks,
                status,
            },
            now,
        );
        ob.closes.push((task.job, task.dir));
        self.slot.current.store(NO_TASK, SeqCst);
        self.slot.stall_reported.store(false, SeqCst);
    }

    fn finish(&mut self, chunks: u32, status: ListingStatus, clock: &dyn Clock) {
        let r = self.running.take().expect("running task");
        let mut ob = self.slot.outbox.lock();
        self.close_locked(&mut ob, &r.task, chunks, status, clock.now());
    }

    fn take_task(&self, sh: &Shared<B>) -> Option<Task<B::Loc>> {
        {
            let mut ob = self.slot.outbox.lock();
            if let Some(t) = ob.tasks.pop() {
                self.slot.current.store(pack(t.job, t.dir), SeqCst);
                return Some(t);
            }
        }
        let t = steal(&sh.priority).or_else(|| steal(&sh.injector))?;
        let _ob = self.slot.outbox.lock();
        self.slot.current.store(pack(t.job, t.dir), SeqCst);
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
            match sh.job(task.job) {
                Some(job) if !job.ended.load(SeqCst) && !job.stopped(task.dir) => {
                    self.running = Some(Running {
                        task,
                        job,
                        stream: None,
                        seq: 0,
                        entries: 0,
                        panicked: 0,
                        read_error: None,
                    });
                }
                _ => {
                    let mut ob = self.slot.outbox.lock();
                    self.close_locked(&mut ob, &task, 0, ListingStatus::Cancelled, clock.now());
                }
            }
            return StepResult::Progress;
        };

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
                        let mut c = empty_chunk(r, ChunkStatus::Ok);
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
                Err(e) => {
                    let chunks = r.seq;
                    self.finish(chunks, ListingStatus::Failed(ErrKind::from(&e)), clock);
                }
            }
            return StepResult::Progress;
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
            let chunks = r.seq;
            self.block.clear();
            self.finish(chunks, status, clock);
            return StepResult::Progress;
        }

        let limits = &sh.limits;
        self.slot.begin_syscall(clock.now());
        let res = r.stream.as_mut().expect("open stream").next_block(
            &mut self.block,
            limits.block_entries,
            limits.block_name_bytes,
        );
        if !matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.slot.syscall_since.store(0, SeqCst);
        }
        let more = match res {
            Ok(more) => more,
            // The call is still in progress as far as the watchdog is concerned.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return StepResult::Blocked,
            Err(e) => {
                let err = ErrKind::from(&e);
                if r.entries == 0 && self.block.entries.is_empty() {
                    let chunks = r.seq;
                    self.finish(chunks, ListingStatus::Failed(err), clock);
                    return StepResult::Progress;
                }
                r.read_error = Some(err);
                false
            }
        };
        let full = self.block.entries.len() >= limits.block_entries
            || self.block.names.len() >= limits.block_name_bytes;
        if full || !more {
            if !self.block.entries.is_empty() {
                self.emit_block(sh, clock);
            }
            if !more {
                let r = self.running.as_ref().expect("running");
                let status = match (r.panicked, r.read_error) {
                    (0, None) => ListingStatus::Complete,
                    (p, e) => ListingStatus::Partial {
                        panicked: p,
                        cancelled: 0,
                        read_error: e,
                    },
                };
                let chunks = r.seq;
                self.finish(chunks, status, clock);
            }
        }
        StepResult::Progress
    }

    /// Turns the current block into a `Chunk`, creating tasks for subdirectories. Tasks go on
    /// the outbox's private stack; they become stealable when the outbox is flushed.
    fn emit_block(&mut self, sh: &Shared<B>, clock: &dyn Clock) {
        let r = self.running.as_mut().expect("running");
        let block = std::mem::take(&mut self.block);
        let (cross, network) = (r.job.opts.cross_mounts, r.job.opts.network);
        let (dir_dev, dir_mount) = (r.task.dev, r.task.mount);
        let (focus, epoch) = (sh.focus.load(SeqCst), sh.focus_epoch.load(SeqCst));
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
        r.entries += files.len() + subdirs.len() + errors.len();
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
                });
            }
        }
        let chunk = Chunk {
            job: r.task.job,
            dir: r.task.dir,
            seq: r.seq,
            root_meta: None,
            probe: None,
            names: block.names,
            files,
            first_subdir: DirId(first),
            subdirs,
            errors,
            status: ChunkStatus::Ok,
        };
        r.seq += 1;
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

fn empty_chunk<B: Backend>(r: &Running<B>, status: ChunkStatus) -> Chunk {
    Chunk {
        job: r.task.job,
        dir: r.task.dir,
        seq: r.seq,
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
