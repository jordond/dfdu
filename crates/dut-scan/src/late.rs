//! Workers give their CPU to an applier preempted while it holds the tree's write lock.
//!
//! With as many workers as cores, the applier shares a core with a worker. The kernel preempts
//! it when its time slice ends, which is often inside one of its 1 ms lock slices
//! (`docs/data-pipeline.md` §5.6), and the worker then runs for its own time slice while the
//! lock stays held: 2-6 ms measured on 4 cores, so 10-14% of slices held the lock for over 2 ms
//! and readers (the UI) waited that long. Raising the applier's priority needs privileges, and
//! lowering the workers' (nice 10) still left 3% of slices over 2 ms.
//!
//! Instead the applier records when its slice should have ended, and each worker checks every
//! few entries it stats ([`crate::Block::push`]). A worker that sees a slice running past that
//! time yields its CPU; on the CPU where the applier waits, that lets the applier run and
//! release the lock. Elsewhere the yield returns at once, or hands the CPU to another runnable
//! thread for a moment.
//!
//! The same happens while the applier waits for the lock. A reader's unlock wakes it, but with
//! every CPU busy it can wait for a worker's time slice to end before it runs: with a reader
//! holding the lock for 5 ms at a time, the applier's p99 wait was 10-12 ms, and 7 ms with
//! workers yielding once it has waited 6 ms.
//!
//! One value per process: with two engines in one process (tests), workers also yield for the
//! other engine's applier, which costs a little time and nothing else.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// When the applier is late, in ns since [`EPOCH`]; 0 when it is not waiting for or holding
/// the lock.
static LATE_AT: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();

fn now_ns() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
}

/// The applier is about to take the write lock, or has taken it, and expects to have it, or to
/// have released it, within `within` unless it is kept off the CPU.
pub fn expect_lock_within(within: Duration) {
    LATE_AT.store(now_ns() + within.as_nanos() as u64, Relaxed);
}

/// The applier is about to release the write lock.
pub fn clear() {
    LATE_AT.store(0, Relaxed);
}

/// Called for every entry a worker stats; looks at the clock every 16 entries (about 20 µs of
/// stats with a warm cache), and only while the applier waits for or holds the lock.
#[inline]
pub(crate) fn yield_if_applier_late(entries: usize) {
    if !entries.is_multiple_of(16) {
        return;
    }
    let at = LATE_AT.load(Relaxed);
    if at != 0 && now_ns() > at {
        std::thread::yield_now();
    }
}
