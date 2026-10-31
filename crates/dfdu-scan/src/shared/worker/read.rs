//! The steps that call the filesystem under the stall watch: open a directory, read a block
//! of it, and stat a block, in the listing task or in a chunk task.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use dfdu_tree::ErrKind;

use super::{InlineBlock, StepResult, Worker, empty_chunk};
use crate::backend::{Backend, DirStream, Raw};
use crate::proto::{ChunkStatus, ListingStatus, Probe, ScanMsg};
use crate::shared::{ChunkWork, Clock, DirParts, Shared, Task, TaskKind};

#[cfg(test)]
mod tests;

impl<B: Backend> Worker<B> {
    /// Opens the listing task's directory. A job root is stat'ed first and gets its mount's
    /// limit here.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs each step only in the state it handles: a task running, with its stream open, its inline block or its chunk"
    )]
    pub(super) fn open_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        let opened = self.slot.watch_io(clock.now(), || {
            let (stream, root_meta) = if r.task.is_root {
                let m = sh.backend.stat_root(&r.task.loc)?;
                r.task.dev = m.dev;
                r.task.mount = m.mount;
                (r.task.storage, r.task.gate) = sh.gate_for(m.mount, m.dev, true);
                r.job.mount_roots.lock().insert((m.dev, m.ino));
                (sh.backend.open(&r.task.loc, r.task.storage)?, Some(m))
            } else {
                (sh.backend.open(&r.task.loc, r.task.storage)?, None)
            };
            // Part of opening, under the same stall watch. A failed probe only loses the record.
            let probe = r.task.probe_subvol.then(|| Probe {
                subvol: sh.backend.subvolume(&r.task.loc).ok().flatten(),
                shares_extents: sh.backend.shares_extents(&r.task.loc),
                profile: if r.task.is_root {
                    sh.backend.data_profile(&r.task.loc)
                } else {
                    None
                },
            });
            Ok((stream, root_meta, probe))
        });
        match opened {
            Ok((stream, root_meta, probe)) => {
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
            // The call is still in progress as far as the watchdog is concerned.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return StepResult::Blocked,
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
    pub(super) fn read_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
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
        let stream = r.stream.as_mut().expect("open stream");
        let res = self.slot.watch_io(clock.now(), || {
            stream.next_raw(limits.block_entries, limits.block_name_bytes)
        });
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
    pub(super) fn push_chunk_task(&mut self, sh: &Shared<B>, raw: Raw<B::Loc>, seq: u32) {
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
            kind: TaskKind::Chunk(Box::new(ChunkWork {
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
    pub(super) fn inline_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        let block = r.inline.as_mut().expect("inline block");
        let out = &mut self.block;
        let res = self.slot.watch_io(clock.now(), || block.raw.stat(out));
        if matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return StepResult::Blocked;
        }
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
    pub(super) fn chunk_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        // Stopped chunk tasks were turned away when taken; once stat'ing has begun it runs to
        // the end, like a blocking call.
        let work = r.task.kind.chunk_mut().expect("chunk task");
        let out = &mut self.block;
        let res = self.slot.watch_io(clock.now(), || work.raw.stat(out));
        if matches!(&res, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return StepResult::Blocked;
        }
        let seq = work.seq;
        match res {
            Ok(()) => self.emit_block(sh, seq, false, clock),
            Err(e) => {
                self.block.clear();
                let r = self.running.as_ref().expect("running");
                let work = r.task.kind.chunk().expect("chunk task");
                work.parts.panicked.fetch_add(1, SeqCst);
                let chunk = empty_chunk(&r.task, seq, ChunkStatus::Panicked(e.to_string()));
                self.slot
                    .outbox
                    .lock()
                    .push(ScanMsg::Chunk(chunk), clock.now());
            }
        }
        let r = self.running.take().expect("running");
        let work = r.task.kind.chunk().expect("chunk task");
        let mut ob = self.slot.outbox.lock();
        let class = work.class.as_deref();
        self.finish_part(&mut ob, &r.task, &work.parts, class, seq + 1, clock.now());
        StepResult::Progress
    }
}
