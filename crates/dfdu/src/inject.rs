//! Test hook: `DFDU_TEST_WORKER_PANIC` (hidden) makes the scan panic in a worker, for
//! `tests/worker_panic.rs`.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use dfdu_scan::{Backend, Forward};

pub fn requested() -> bool {
    std::env::var_os("DFDU_TEST_WORKER_PANIC").is_some()
}

/// Forwards to `B`, except that opening the second directory (the first below the root) panics.
pub struct PanicOnSecondOpen<B> {
    inner: B,
    opens: AtomicUsize,
}

impl<B> PanicOnSecondOpen<B> {
    pub fn new(inner: B) -> Self {
        PanicOnSecondOpen {
            inner,
            opens: AtomicUsize::new(0),
        }
    }
}

impl<B: Backend> Forward for PanicOnSecondOpen<B> {
    type Inner = B;
    type Stream = B::Stream;

    fn inner(&self) -> &B {
        &self.inner
    }
    fn open(&self, loc: &B::Loc, storage: dfdu_sys::Storage) -> io::Result<B::Stream> {
        if self.opens.fetch_add(1, SeqCst) == 1 {
            panic!("injected worker panic");
        }
        self.inner.open(loc, storage)
    }
}
