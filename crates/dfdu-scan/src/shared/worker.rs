//! The worker step function: take a task, open its directory, read a block or stat one, and
//! append what that produced to the worker's outbox.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use dfdu_tree::{ErrKind, Kind};

use super::{
    ChunkWork, Clock, DirParts, JobState, LISTING_PART, NO_TASK, Outbox, Shared, Sink, Task,
    WorkerSlot, pack, panic_message,
};
use crate::FsClass;
use crate::backend::{Backend, Block, DirStream, Raw};
use crate::classify::Listing;
use crate::proto::{
    Chunk, ChunkStatus, DirId, EntryError, FileRec, ListingStatus, Probe, ScanMsg, SkipReason,
    SubdirRec,
};
use crate::sync::{Injector, Steal};

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

/// A block a listing task is stat'ing itself.
struct InlineBlock<L> {
    raw: Raw<L>,
    seq: u32,
    /// More blocks follow this one.
    more: bool,
    /// The block is the whole listing.
    whole: bool,
}

struct Running<B: Backend> {
    task: Task<B::Loc>,
    job: Arc<JobState>,
    stream: Option<B::Stream>,
    /// Kept across "not ready" returns, and set while stat'ing so a panic there costs only
    /// this block.
    inline: Option<InlineBlock<B::Loc>>,
    seq: u32,
    /// Entries read so far; a read error before any entry means the directory failed.
    entries: usize,
    panicked: u32,
    read_error: Option<ErrKind>,
    /// Set once the listing hands a block to a chunk task.
    parts: Option<Arc<DirParts>>,
    /// The directory's classification, shared with its chunk tasks.
    class: Option<Arc<Listing>>,
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
    /// caught and recorded against the directory.
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
            let class = work.class.as_deref();
            self.finish_part(&mut ob, &r.task, &work.parts, class, work.seq + 1, now);
            return;
        }
        if let Some(block) = r.inline.take() {
            let chunk = empty_chunk(&r.task, block.seq, ChunkStatus::Panicked(msg));
            r.panicked += 1;
            self.slot.outbox.lock().push(ScanMsg::Chunk(chunk), now);
            let status = Self::ended_status(&r);
            self.running = Some(r);
            if !block.more {
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
            || ob.gated_task
            || (sh.idle.load(SeqCst) > 0 && (ob.tasks.len() > 1 || ob.has_chunk_task))
    }

    /// Flushes the outbox. In blocking mode, waits for budget.
    pub fn flush(&mut self, sh: &Shared<B>, sink: &dyn Sink) -> bool {
        let mut ob = self.slot.outbox.lock();
        if ob.is_empty() {
            return true;
        }
        sh.flush_locked(&mut ob, sink, self.blocking)
    }

    /// Ends the current part: it no longer counts as running, and the next flush reports it
    /// done.
    fn end_part(&self, ob: &mut Outbox<B::Loc>, task: &Task<B::Loc>, part: u32) {
        // The permit passes to the oldest task waiting for one, which this worker runs next.
        if let Some(g) = ob.permit.take() {
            debug_assert!(ob.handoff.is_none());
            ob.handoff = g.leave();
        }
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
        class: Option<&Listing>,
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
                    classes: class.map(Listing::finish).unwrap_or_default(),
                },
                now,
            );
        }
        self.end_part(ob, task, part);
    }

    /// Ends the listing task with `status`: sends the `Close`, or leaves it to the last chunk
    /// task still running.
    #[expect(
        clippy::expect_used,
        reason = "called only by the steps of a running listing task"
    )]
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
                        classes: r.class.as_deref().map(Listing::finish).unwrap_or_default(),
                    },
                    now,
                );
                self.end_part(&mut ob, &r.task, LISTING_PART);
            }
            Some(parts) => {
                *parts.listing.lock() = Some(status);
                parts.chunks.store(r.seq, SeqCst);
                let class = r.class.as_deref();
                self.finish_part(&mut ob, &r.task, parts, class, LISTING_PART, now);
            }
        }
    }

    /// The next task: one handed over with a permit, then the newest own task, then the
    /// priority and normal queues. A task whose mount is at its limit waits in the mount's gate
    /// if it was published, or stays on the own stack, published at the next flush, if not;
    /// either way the worker looks further.
    #[expect(
        clippy::expect_used,
        reason = "the last task on the stack was looked at under the same lock"
    )]
    fn take_task(&self, sh: &Shared<B>) -> Option<Task<B::Loc>> {
        let set = |ob: &mut Outbox<B::Loc>, t: &Task<B::Loc>, permit: bool| {
            self.slot.current.store(pack(t.job, t.dir), SeqCst);
            let part = t.chunk.as_ref().map_or(LISTING_PART, |c| c.seq + 1);
            self.slot.part.store(part, SeqCst);
            if permit {
                ob.permit.clone_from(&t.gate);
            }
        };
        {
            let mut ob = self.slot.outbox.lock();
            if let Some(t) = ob.handoff.take() {
                set(&mut ob, &t, true);
                return Some(t);
            }
            let runnable = ob.tasks.last().map(|t| match &t.gate {
                None => Some(false),
                Some(g) => g.try_enter().then_some(true),
            });
            match runnable {
                Some(Some(permit)) => {
                    let t = ob.tasks.pop().expect("task");
                    set(&mut ob, &t, permit);
                    return Some(t);
                }
                Some(None) => ob.gated_task = true,
                None => {}
            }
        }
        loop {
            let t = steal(&sh.priority).or_else(|| steal(&sh.injector))?;
            let mut ob = self.slot.outbox.lock();
            let Some(g) = t.gate.clone() else {
                set(&mut ob, &t, false);
                return Some(t);
            };
            if let Some(t) = g.enter_or_wait(t) {
                set(&mut ob, &t, true);
                return Some(t);
            }
        }
    }

    /// A task handed over with a permit is waiting to run here; the worker must not exit.
    pub fn has_handoff(&self) -> bool {
        self.slot.outbox.lock().handoff.is_some()
    }

    /// The location of the task handed over to this worker, for tests.
    pub fn handoff_loc(&self) -> Option<B::Loc> {
        self.slot
            .outbox
            .lock()
            .handoff
            .as_ref()
            .map(|t| t.loc.clone())
    }

    /// What the worker runs now, for tests: the task's location, whether it is a job root,
    /// and whether it holds a permit of its mount's limit.
    pub fn running_part(&self) -> Option<(B::Loc, bool, bool)> {
        let r = self.running.as_ref()?;
        let permit = self.slot.outbox.lock().permit.is_some();
        Some((r.task.loc.clone(), r.task.is_root, permit))
    }

    fn step_inner(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        let Some(r) = self.running.as_ref() else {
            return self.start_task(sh, sink, clock);
        };
        if r.task.chunk.is_some() {
            return self.chunk_step(sh, clock);
        }
        if r.stream.is_none() {
            return self.open_step(sh, clock);
        }
        // A block being stat'ed is finished first, like a blocking call that has begun.
        if r.inline.is_some() {
            return self.inline_step(sh, clock);
        }
        self.read_step(sh, clock)
    }

    /// Takes the next task and starts it, or turns it away if its job ended or its directory
    /// was stopped. With no task, flushes the outbox.
    fn start_task(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        let Some(mut task) = self.take_task(sh) else {
            let mut ob = self.slot.outbox.lock();
            if !ob.is_empty() {
                sh.flush_locked(&mut ob, sink, self.blocking);
            }
            return StepResult::Idle;
        };
        let job = sh
            .job(task.job)
            .filter(|j| !j.ended.load(SeqCst) && !j.stopped(task.dir));
        let Some(job) = job else {
            self.cancel_task(&task, clock);
            return StepResult::Progress;
        };
        let class = match (&task.chunk, task.class.take(), &job.opts.class) {
            (Some(work), _, _) => work.class.clone(),
            (None, Some(tc), Some(opts)) => {
                Some(Arc::new(Listing::new(opts.rules.clone(), task.dir, *tc)))
            }
            _ => None,
        };
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
            class,
        });
        StepResult::Progress
    }

    /// Ends a task that will not run: a chunk task still sends its one chunk, so the
    /// directory's count is right; a listing task closes its directory as cancelled.
    fn cancel_task(&self, task: &Task<B::Loc>, clock: &dyn Clock) {
        let mut ob = self.slot.outbox.lock();
        let now = clock.now();
        if let Some(work) = &task.chunk {
            let chunk = empty_chunk(task, work.seq, ChunkStatus::Cancelled);
            ob.push(ScanMsg::Chunk(chunk), now);
            work.parts.cancelled.fetch_add(1, SeqCst);
            let class = work.class.as_deref();
            self.finish_part(&mut ob, task, &work.parts, class, work.seq + 1, now);
        } else {
            ob.push(
                ScanMsg::Close {
                    job: task.job,
                    dir: task.dir,
                    chunks: 0,
                    status: ListingStatus::Cancelled,
                    classes: Vec::new(),
                },
                now,
            );
            self.end_part(&mut ob, task, LISTING_PART);
        }
    }

    /// Opens the listing task's directory. A job root is stat'ed first and gets its mount's
    /// limit here.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
    fn open_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        self.slot.begin_syscall(clock.now());
        let opened = if r.task.is_root {
            sh.backend.stat_root(&r.task.loc).and_then(|m| {
                r.task.dev = m.dev;
                r.task.mount = m.mount;
                (r.task.storage, r.task.gate) = sh.gate_for(m.mount, m.dev, true);
                r.job.mount_roots.lock().insert((m.dev, m.ino));
                sh.backend
                    .open(&r.task.loc, r.task.storage)
                    .map(|s| (s, Some(m)))
            })
        } else {
            sh.backend
                .open(&r.task.loc, r.task.storage)
                .map(|s| (s, None))
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
        StepResult::Progress
    }

    /// Reads the next block of the open directory, and hands it to a chunk task or stats it
    /// here. A stopped listing ends with what it has read.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
    fn read_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
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
        let whole = r.entries == 0 && !more;
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
            // More blocks follow: another worker can stat this one.
            self.push_chunk_task(sh, raw, seq);
            return StepResult::Progress;
        }
        r.inline = Some(InlineBlock {
            raw,
            seq,
            more,
            whole,
        });
        self.inline_step(sh, clock)
    }

    /// Makes block `seq` of the listing a chunk task on the outbox's private stack.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
    fn push_chunk_task(&mut self, sh: &Shared<B>, raw: Raw<B::Loc>, seq: u32) {
        let r = self.running.as_mut().expect("running");
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
            chunk: Some(Box::new(ChunkWork {
                raw,
                seq,
                parts,
                class: r.class.clone(),
            })),
            gate: r.task.gate.clone(),
            storage: r.task.storage,
            class: None,
        };
        let mut ob = self.slot.outbox.lock();
        sh.outstanding.fetch_add(1, SeqCst);
        sh.submitted_seq.fetch_add(1, SeqCst);
        sh.chunk_tasks.fetch_add(1, SeqCst);
        ob.tasks.push(task);
        ob.has_chunk_task = true;
    }

    /// The listing task stats a block itself.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
    fn inline_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        let block = r.inline.as_mut().expect("inline block");
        self.slot.begin_syscall(clock.now());
        let res = block.raw.stat(&mut self.block);
        if matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return StepResult::Blocked;
        }
        self.slot.syscall_since.store(0, SeqCst);
        let block = r.inline.take().expect("inline block");
        if let Err(e) = res {
            // Only "not ready" is expected; anything else loses the block, as a panic would.
            let chunk = empty_chunk(&r.task, block.seq, ChunkStatus::Panicked(e.to_string()));
            r.panicked += 1;
            self.block.clear();
            self.slot
                .outbox
                .lock()
                .push(ScanMsg::Chunk(chunk), clock.now());
        } else {
            self.emit_block(sh, block.seq, block.whole, clock);
        }
        if !block.more {
            let status = Self::ended_status(self.running.as_ref().expect("running"));
            self.finish_listing(status, clock);
        }
        StepResult::Progress
    }

    /// A chunk task: stat its block and send it as one chunk.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
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
            Ok(()) => self.emit_block(sh, seq, false, clock),
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
        let class = work.class.as_deref();
        self.finish_part(&mut ob, &r.task, &work.parts, class, seq + 1, clock.now());
        StepResult::Progress
    }

    /// Turns the stat'ed block into chunk `seq`, creating tasks for subdirectories. Tasks go
    /// on the outbox's private stack; they become stealable when the outbox is flushed.
    /// `whole` is set when the block is the directory's whole listing.
    #[expect(clippy::expect_used, reason = "called only while a task runs")]
    #[expect(
        clippy::too_many_lines,
        reason = "one pass over the block's entries, building the chunk and the subdirectory tasks together"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a block holds at most `block_entries` entries, far below `u32::MAX`"
    )]
    fn emit_block(&mut self, sh: &Shared<B>, seq: u32, whole: bool, clock: &dyn Clock) {
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
                    let (storage, gate) = match &loc {
                        Some(_) if mount_root => sh.gate_for(m.mount, m.dev, false),
                        Some(_) => (r.task.storage, r.task.gate.clone()),
                        None => (dfdu_sys::Storage::Unknown, None),
                    };
                    child_locs
                        .push(loc.map(|l| (l, m.dev, m.mount, m.dev != dir_dev, gate, storage)));
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
        let mut class = r.class.as_ref().map(|l| {
            let others: Vec<_> = errors.iter().map(|e: &EntryError| e.name).collect();
            let (slot, loc) = (&self.slot, &r.task.loc);
            // A marker file is read under the stall watch, like any other call.
            let read = |name: &[u8], len: usize| {
                slot.begin_syscall(clock.now());
                let got = sh.backend.read_prefix(loc, name, len);
                slot.syscall_since.store(0, SeqCst);
                got
            };
            // Following a link may wait on another mount, so it is watched too.
            let missing = |name: &[u8]| {
                slot.begin_syscall(clock.now());
                let got = sh.backend.link_target_missing(loc, name);
                slot.syscall_since.store(0, SeqCst);
                got
            };
            l.block(
                &block.names,
                &files,
                &subdirs,
                &others,
                DirId(first),
                whole,
                &read,
                &missing,
            )
        });
        let mut tasks = Vec::new();
        for (i, loc) in child_locs.into_iter().enumerate() {
            if let Some((loc, dev, mount, probe_subvol, gate, storage)) = loc {
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
                    gate,
                    storage,
                    class: class.as_mut().and_then(|c| c.subdirs[i].take()),
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
            marks: class.map(|c| c.marks).unwrap_or_default(),
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
        marks: Vec::new(),
        status,
    }
}

fn steal<T>(inj: &Injector<T>) -> Option<T> {
    loop {
        match inj.steal() {
            Steal::Success(t) => return Some(t),
            Steal::Empty => return None,
            Steal::Retry => {}
        }
    }
}
