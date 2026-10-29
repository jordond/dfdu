//! Helpers shared by the unit tests of the scanner's shared state and its worker: a backend
//! over real directories, an inbox, and a worker run until it has nothing left to do.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;

use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{DirId, Meta};

use super::{Clock, Shared, Sink, StepResult, Task, Worker};
use crate::backend::{Backend, PortableBackend};
use crate::proto::{Batch, ScanMsg};
use crate::sync::{AtomicU64, Mutex};

/// The portable backend, with mount classes from `classes` (by mount id) instead of the
/// system's mount table.
#[derive(Default)]
pub(crate) struct Local {
    inner: PortableBackend,
    classes: HashMap<u64, FsClass>,
}

impl Local {
    pub(crate) fn with_classes(classes: HashMap<u64, FsClass>) -> Self {
        Local {
            inner: PortableBackend::default(),
            classes,
        }
    }
}

impl Backend for Local {
    type Loc = PathBuf;
    type Stream = <PortableBackend as Backend>::Stream;

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        self.inner.loc_from_path(path)
    }
    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        self.inner.stat_root(loc)
    }
    fn open(&self, loc: &PathBuf, storage: Storage) -> io::Result<Self::Stream> {
        self.inner.open(loc, storage)
    }
    fn mount_class(&self, mount: u64, _dev: u64) -> Option<FsClass> {
        self.classes.get(&mount).copied()
    }
}

/// Every message sent, in order, control messages included.
#[derive(Default)]
pub(crate) struct Inbox(Mutex<Vec<ScanMsg>>);

impl Inbox {
    pub(crate) fn take(&self) -> Vec<ScanMsg> {
        std::mem::take(&mut *self.0.lock())
    }
}

impl Sink for Inbox {
    fn try_send(&self, batch: Batch) -> Result<(), Batch> {
        self.send(batch);
        Ok(())
    }
    fn send(&self, batch: Batch) {
        self.0.lock().extend(batch.msgs);
    }
    fn send_control(&self, msg: ScanMsg) {
        self.0.lock().push(msg);
    }
}

/// A clock that moves one nanosecond per reading, from 1.
#[derive(Default)]
pub(crate) struct Ticks(AtomicU64);

impl Clock for Ticks {
    fn now(&self) -> u64 {
        self.0.fetch_add(1, SeqCst) + 1
    }
}

/// Steps `w` until it is idle, and returns what it sent.
///
/// # Panics
///
/// If it is not idle after many steps.
pub(crate) fn run<B: Backend>(sh: &Shared<B>, w: &mut Worker<B>, inbox: &Inbox) -> Vec<ScanMsg> {
    let clock = Ticks::default();
    let idle = (0..100_000).any(|_| w.step(sh, inbox, &clock) == StepResult::Idle);
    assert!(idle, "the worker never ran out of work");
    inbox.take()
}

/// The `Close` of directory `dir`.
///
/// # Panics
///
/// If there is none.
pub(crate) fn close_of(msgs: &[ScanMsg], dir: DirId) -> &ScanMsg {
    let close = msgs
        .iter()
        .find(|m| matches!(m, ScanMsg::Close { dir: d, .. } if *d == dir));
    close.unwrap_or_else(|| panic!("no close for dir {}", dir.0))
}

/// A listing task of job 1 for directory `dir` at `loc`, on device `dev` and mount `mount`.
pub(crate) fn task<L>(dir: DirId, loc: L, dev: u64, mount: u64) -> Task<L> {
    Task {
        job: dfdu_tree::JobId(1),
        dir,
        loc,
        focus_epoch: 0,
        is_root: false,
        dev,
        mount,
        probe_subvol: false,
        chunk: None,
        gate: None,
        storage: Storage::Unknown,
        class: None,
    }
}
