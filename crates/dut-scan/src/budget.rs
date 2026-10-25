use parking_lot::{Condvar, Mutex};

/// Counting semaphore on bytes in transit to the applier (`docs/data-pipeline.md` §4.3).
pub struct Budget {
    capacity: usize,
    used: Mutex<usize>,
    freed: Condvar,
}

impl Budget {
    pub fn new(capacity: usize) -> Self {
        Budget {
            capacity,
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn used(&self) -> usize {
        *self.used.lock()
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
        n
    }

    pub fn try_acquire(&self, n: usize) -> Option<usize> {
        let n = self.clamp(n);
        let mut used = self.used.lock();
        if *used + n > self.capacity {
            return None;
        }
        *used += n;
        Some(n)
    }

    pub fn release(&self, n: usize) {
        let mut used = self.used.lock();
        debug_assert!(*used >= n, "budget released more than acquired");
        *used = used.saturating_sub(n);
        self.freed.notify_all();
    }
}
