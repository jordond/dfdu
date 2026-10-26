//! Per-mount concurrency limits (`docs/data-pipeline.md` §4.5): at most `limit` parts of
//! directories on one mount run at once, so a spinning disk is not read by every worker and a
//! hung network mount holds only its own permits.
//!
//! A gate never blocks a worker. A task that finds its mount at the limit is kept in the gate's
//! pending list and the worker looks for other work; the part that next releases a permit takes
//! the oldest pending task with the permit (a handoff). A permit is held from taking a task to
//! the end of the part. The watchdog takes back the permit of a part stuck in a filesystem call
//! and requeues one pending task, so stuck calls do not keep the mount's other directories
//! waiting.
//!
//! Every change happens under the gate's lock. While tasks are pending, every free permit has
//! a requeued task on its way to a worker, so no pending task is left without a permit to come.

use std::collections::VecDeque;

use crate::sync::Mutex;

/// See the module docs.
pub struct Gate<T> {
    /// Mount id, or 0 with `dev` when the backend has no mount ids.
    pub mount: u64,
    pub dev: u64,
    pub limit: usize,
    state: Mutex<State<T>>,
}

struct State<T> {
    running: usize,
    pending: VecDeque<T>,
    deferred: u64,
    revoked: u64,
    peak: usize,
}

/// A gate's counters, for tests and `--debug-stats`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateStats {
    pub mount: u64,
    pub dev: u64,
    pub limit: usize,
    /// Permits held now.
    pub running: usize,
    /// Tasks waiting for a permit now.
    pub pending: usize,
    /// Times a task had to wait for a permit.
    pub deferred: u64,
    /// Permits taken back from stuck parts.
    pub revoked: u64,
    /// Most permits held at once.
    pub peak: usize,
}

impl<T> Gate<T> {
    pub fn new(mount: u64, dev: u64, limit: usize) -> Self {
        assert!(limit > 0, "a gate needs at least one permit");
        Gate {
            mount,
            dev,
            limit,
            state: Mutex::new(State {
                running: 0,
                pending: VecDeque::new(),
                deferred: 0,
                revoked: 0,
                peak: 0,
            }),
        }
    }

    /// Takes a permit if one is free.
    pub fn try_enter(&self) -> bool {
        let mut s = self.state.lock();
        s.try_take(self.limit)
    }

    /// Takes a permit for `t`, returning it, or keeps `t` pending until a permit frees. Only a
    /// published task may be kept: whoever takes it next may run it at once.
    pub fn enter_or_wait(&self, t: T) -> Option<T> {
        let mut s = self.state.lock();
        if s.try_take(self.limit) {
            Some(t)
        } else {
            s.deferred += 1;
            s.pending.push_back(t);
            None
        }
    }

    /// Releases a permit. A pending task is returned holding it, to be run next by the caller.
    pub fn leave(&self) -> Option<T> {
        let mut s = self.state.lock();
        let next = s.pending.pop_front();
        if next.is_none() {
            s.running -= 1;
        }
        next
    }

    /// Takes back the permit of a part stuck in a call; returns a pending task to requeue,
    /// which takes the permit when a worker takes it. `None` if the lock is busy (the watchdog
    /// never blocks; it retries next round).
    pub fn revoke(&self) -> Option<Option<T>> {
        let mut s = self.state.try_lock()?;
        s.running -= 1;
        s.revoked += 1;
        Some(s.pending.pop_front())
    }

    pub fn stats(&self) -> GateStats {
        let s = self.state.lock();
        GateStats {
            mount: self.mount,
            dev: self.dev,
            limit: self.limit,
            running: s.running,
            pending: s.pending.len(),
            deferred: s.deferred,
            revoked: s.revoked,
            peak: s.peak,
        }
    }
}

impl<T> State<T> {
    fn try_take(&mut self, limit: usize) -> bool {
        if self.running >= limit {
            return false;
        }
        self.running += 1;
        self.peak = self.peak.max(self.running);
        true
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn waits_hand_over_and_revokes() {
        let g = Gate::new(7, 0, 2);
        assert_eq!(g.enter_or_wait(1), Some(1));
        assert!(g.try_enter());
        assert!(!g.try_enter());
        assert_eq!(g.enter_or_wait(3), None);
        assert_eq!(g.enter_or_wait(4), None);
        // The first release hands its permit to the oldest pending task.
        assert_eq!(g.leave(), Some(3));
        // A stuck part's permit is taken back; the next pending task is requeued.
        assert_eq!(g.revoke(), Some(Some(4)));
        assert_eq!(g.enter_or_wait(4), Some(4));
        assert_eq!(g.leave(), None);
        assert_eq!(g.leave(), None);
        let s = g.stats();
        assert_eq!(
            (s.running, s.pending, s.deferred, s.revoked, s.peak),
            (0, 0, 2, 1, 2)
        );
    }
}
