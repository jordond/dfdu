use std::sync::atomic::Ordering::Relaxed;

use crate::sync::{AtomicUsize, Condvar, Mutex};

/// Counting semaphore on bytes in transit to the applier.
pub struct Budget {
    capacity: usize,
    used: Mutex<usize>,
    peak: AtomicUsize,
    freed: Condvar,
}

impl Budget {
    pub fn new(capacity: usize) -> Self {
        Budget {
            capacity,
            used: Mutex::new(0),
            peak: AtomicUsize::new(0),
            freed: Condvar::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn used(&self) -> usize {
        *self.used.lock()
    }

    /// The most bytes in transit at once so far.
    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }

    /// A request larger than the capacity is clamped to it, so it can still proceed once the
    /// budget is empty; the returned amount is what must be released.
    fn clamp(&self, n: usize) -> usize {
        n.min(self.capacity)
    }

    pub fn acquire(&self, n: usize) -> usize {
        let n = self.clamp(n);
        let mut used = self.used.lock();
        while *used + n > self.capacity {
            self.freed.wait(&mut used);
        }
        *used += n;
        self.peak.fetch_max(*used, Relaxed);
        n
    }

    pub fn try_acquire(&self, n: usize) -> Option<usize> {
        let n = self.clamp(n);
        let mut used = self.used.lock();
        if *used + n > self.capacity {
            return None;
        }
        *used += n;
        self.peak.fetch_max(*used, Relaxed);
        Some(n)
    }

    pub fn release(&self, n: usize) {
        let mut used = self.used.lock();
        debug_assert!(*used >= n, "budget released more than acquired");
        *used = used.saturating_sub(n);
        self.freed.notify_all();
    }
}
