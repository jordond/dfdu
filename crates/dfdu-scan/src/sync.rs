//! Synchronisation types used by the scanner's shared state, so the same code runs under
//! `loom`. Normal builds use `std` atomics, `parking_lot` locks and `crossbeam-deque`'s
//! injector directly. With `--cfg loom`, the atomics, locks and thread parking are `loom`'s,
//! behind the `parking_lot` API, and the injector is a locked queue:
//! `loom` cannot run `crossbeam-deque`, and the protocol around the injector only needs it to
//! be a linearizable FIFO.

pub use crossbeam_deque::Steal;

#[cfg(loom)]
pub use imp_loom::*;
#[cfg(not(loom))]
pub use imp_std::*;

#[cfg(not(loom))]
mod imp_std {
    pub use crossbeam_deque::Injector;
    pub use parking_lot::{Condvar, Mutex, RwLock};
    pub use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
    pub use std::thread::{Thread, current};

    pub fn park_timeout(d: std::time::Duration) {
        std::thread::park_timeout(d);
    }
}

#[cfg(loom)]
mod imp_loom {
    use std::collections::VecDeque;
    use std::ops::{Deref, DerefMut};

    use super::Steal;

    pub use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
    pub use loom::thread::{Thread, current};

    /// Under `loom` a timed park is an untimed one, so a lost wake shows up as a deadlock.
    pub fn park_timeout(_: std::time::Duration) {
        loom::thread::park();
    }

    #[derive(Debug, Default)]
    pub struct Mutex<T>(loom::sync::Mutex<T>);

    pub struct MutexGuard<'a, T>(Option<loom::sync::MutexGuard<'a, T>>);

    impl<T> Mutex<T> {
        pub fn new(v: T) -> Self {
            Mutex(loom::sync::Mutex::new(v))
        }
        pub fn lock(&self) -> MutexGuard<'_, T> {
            MutexGuard(Some(self.0.lock().expect("poisoned")))
        }
        pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
            self.0.try_lock().ok().map(|g| MutexGuard(Some(g)))
        }
    }

    impl<T> Deref for MutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.0.as_ref().expect("guard")
        }
    }

    impl<T: std::fmt::Debug> std::fmt::Debug for MutexGuard<'_, T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            (**self).fmt(f)
        }
    }

    impl<T> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            self.0.as_mut().expect("guard")
        }
    }

    #[derive(Debug, Default)]
    pub struct Condvar(loom::sync::Condvar);

    impl Condvar {
        pub fn new() -> Self {
            Condvar(loom::sync::Condvar::new())
        }
        pub fn wait<T>(&self, g: &mut MutexGuard<'_, T>) {
            let inner = g.0.take().expect("guard");
            g.0 = Some(self.0.wait(inner).expect("poisoned"));
        }
        pub fn notify_all(&self) {
            self.0.notify_all();
        }
    }

    #[derive(Debug, Default)]
    pub struct RwLock<T>(loom::sync::RwLock<T>);

    impl<T> RwLock<T> {
        pub fn new(v: T) -> Self {
            RwLock(loom::sync::RwLock::new(v))
        }
        pub fn read(&self) -> loom::sync::RwLockReadGuard<'_, T> {
            self.0.read().expect("poisoned")
        }
        pub fn write(&self) -> loom::sync::RwLockWriteGuard<'_, T> {
            self.0.write().expect("poisoned")
        }
    }

    /// `crossbeam_deque::Injector` as a locked FIFO.
    pub struct Injector<T>(Mutex<VecDeque<T>>);

    impl<T> Default for Injector<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T> Injector<T> {
        pub fn new() -> Self {
            Injector(Mutex::new(VecDeque::new()))
        }
        pub fn push(&self, t: T) {
            self.0.lock().push_back(t);
        }
        pub fn steal(&self) -> Steal<T> {
            match self.0.lock().pop_front() {
                Some(t) => Steal::Success(t),
                None => Steal::Empty,
            }
        }
        pub fn is_empty(&self) -> bool {
            self.0.lock().is_empty()
        }
        pub fn len(&self) -> usize {
            self.0.lock().len()
        }
    }
}

/// Threads waiting for tasks to be published. A thread registers before its last look at the
/// queues and parks only if they are still empty, so a publication after that look finds it
/// registered and unparks it; an unpark that comes before the park is kept by the thread's park
/// token. Several wakes before a park coalesce into one.
pub struct Sleepers {
    threads: Mutex<Vec<Thread>>,
}

impl Default for Sleepers {
    fn default() -> Self {
        Sleepers {
            threads: Mutex::new(Vec::new()),
        }
    }
}

impl Sleepers {
    /// Registers the calling thread, then parks it for at most `timeout` unless `ready()`
    /// says there is work. A registration left behind when `ready()` is true or the park times
    /// out costs one early return from a later park.
    pub fn sleep(&self, ready: impl FnOnce() -> bool, timeout: std::time::Duration) {
        self.threads.lock().push(current());
        if !ready() {
            park_timeout(timeout);
        }
    }

    pub fn wake_all(&self) {
        for t in self.threads.lock().drain(..) {
            t.unpark();
        }
    }
}
