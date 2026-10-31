//! The worker step function: take a task, open its directory, read a block or stat one, and
//! append what that produced to the worker's outbox. Taking, starting and finishing tasks are
//! here; the filesystem steps are in `read`, and what a block becomes in `emit`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use dfdu_tree::ErrKind;

use super::{
    Clock, DirParts, JobState, LISTING_PART, NO_TASK, Outbox, Shared, Sink, Task, WorkerSlot, pack,
    panic_message,
};
use crate::backend::{Backend, Block, Raw};
use crate::classify::Listing;
use crate::proto::{Chunk, ChunkStatus, DirId, ListingStatus, ScanMsg};
use crate::sync::{Injector, Steal};

mod emit;
mod read;
mod verify;

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

/// The task a worker runs, as [`Worker::running_part`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningPart<L> {
    /// Where the task reads.
    pub loc: L,
    /// Whether it is a job root.
    pub is_root: bool,
    /// Whether the worker holds a permit of the mount's limit for it.
    pub has_permit: bool,
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
                self.slot.end_syscall();
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
        if let Some(work) = r.task.verify.take() {
            self.report_verify(&r.task, *work, false, clock);
            return;
        }
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
        self.slot.end_syscall();
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

    /// What the worker runs now, for tests.
    pub fn running_part(&self) -> Option<RunningPart<B::Loc>> {
        let r = self.running.as_ref()?;
        let has_permit = self.slot.outbox.lock().permit.is_some();
        Some(RunningPart {
            loc: r.task.loc.clone(),
            is_root: r.task.is_root,
            has_permit,
        })
    }

    fn step_inner(&mut self, sh: &Shared<B>, sink: &dyn Sink, clock: &dyn Clock) -> StepResult {
        let Some(r) = self.running.as_ref() else {
            return self.start_task(sh, sink, clock);
        };
        if r.task.verify.is_some() {
            return self.verify_step(sh, clock);
        }
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
            self.cancel_task(task, clock);
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
    /// directory's count is right; a listing task closes its directory as cancelled; a batch
    /// of a resume's check reports that it stat'ed nothing.
    fn cancel_task(&self, mut task: Task<B::Loc>, clock: &dyn Clock) {
        if let Some(work) = task.verify.take() {
            self.report_verify(&task, *work, false, clock);
            return;
        }
        let task = &task;
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
