//! Test hook: `DUT_TEST_WORKER_PANIC` (hidden) makes the scan panic in a worker, for
//! `tests/worker_panic.rs`.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use dut_scan::{Backend, FsClass};
use dut_tree::{Meta, Subvol};

/// Whether the environment asks for a worker panic.
pub fn requested() -> bool {
    std::env::var_os("DUT_TEST_WORKER_PANIC").is_some()
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

impl<B: Backend> Backend for PanicOnSecondOpen<B> {
    type Loc = B::Loc;
    type Stream = B::Stream;

    fn loc_from_path(&self, path: &Path) -> B::Loc {
        self.inner.loc_from_path(path)
    }
    fn stat_root(&self, loc: &B::Loc) -> io::Result<Meta> {
        self.inner.stat_root(loc)
    }
    fn open(&self, loc: &B::Loc, storage: dut_sys::Storage) -> io::Result<B::Stream> {
        if self.opens.fetch_add(1, SeqCst) == 1 {
            panic!("injected worker panic");
        }
        self.inner.open(loc, storage)
    }
    fn subvolume(&self, loc: &B::Loc) -> io::Result<Option<Subvol>> {
        self.inner.subvolume(loc)
    }
    fn shares_extents(&self, loc: &B::Loc) -> bool {
        self.inner.shares_extents(loc)
    }
    fn data_profile(&self, loc: &B::Loc) -> Option<(&'static str, f64)> {
        self.inner.data_profile(loc)
    }
    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        self.inner.duplicate_mounts(scope)
    }
    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        self.inner.mount_class(mount, dev)
    }
}
