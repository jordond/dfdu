//! Shutdown waits for workers within its bound and leaves a worker stuck in a filesystem call
//! behind.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{Backend, Forward, JobOptions, PortableBackend};

/// Opens wait while the gate is closed.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
    /// An open is waiting at the gate.
    waiting: AtomicBool,
}

impl Gate {
    fn pass(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            self.waiting.store(true, SeqCst);
            open = self.opened.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// A portable backend whose second open waits at the gate: a worker stuck in a filesystem call.
struct Stuck {
    inner: PortableBackend,
    opens: AtomicUsize,
    gate: Arc<Gate>,
}

impl Forward for Stuck {
    type Inner = PortableBackend;
    type Stream = <PortableBackend as Backend>::Stream;

    fn inner(&self) -> &PortableBackend {
        &self.inner
    }
    fn open(&self, loc: &PathBuf, storage: dfdu_sys::Storage) -> io::Result<Self::Stream> {
        if self.opens.fetch_add(1, SeqCst) == 1 {
            self.gate.pass();
        }
        self.inner.open(loc, storage)
    }
}

#[test]
fn shutdown_leaves_a_stuck_worker_within_its_bound() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("a")).unwrap();
    let gate = Arc::new(Gate::default());
    let engine = Engine::start(
        Stuck {
            inner: PortableBackend::default(),
            opens: AtomicUsize::new(0),
            gate: gate.clone(),
        },
        EngineOptions {
            workers: 2,
            measure_workers: 0,
            classifiers: 0,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: tmp.path().to_path_buf().into(),
        opts: JobOptions::default(),
    });
    let t0 = Instant::now();
    while !gate.waiting.load(SeqCst) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "no worker reached the gate"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let wait = Duration::from_millis(200);
    let t = Instant::now();
    engine.shutdown(wait);
    let took = t.elapsed();
    gate.release();
    assert!(
        took >= wait,
        "returned after {took:?}, before `wait` passed"
    );
    // The bound, plus the applier's stop and scheduling.
    assert!(took < wait + Duration::from_secs(1), "took {took:?}");
}
