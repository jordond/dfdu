//! The watchdog step function: report stalled filesystem calls, flush outboxes left waiting,
//! and send `Quiescent` when nothing is outstanding.

use std::sync::atomic::Ordering::SeqCst;

use super::{NO_TASK, Shared, Sink, unpack};
use crate::backend::Backend;
use crate::proto::ScanMsg;

#[cfg(test)]
mod tests;

impl<B: Backend> Shared<B> {
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
