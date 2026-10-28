//! A reader holding the read lock for long stretches (a slow renderer) delays the applier between
//! slices but cannot stop a scan or change its result.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions, Timing};
use dfdu_scan::{JobOptions, PortableBackend};
use tempfile::TempDir;

mod common;
use common::tempdir;

fn tree() -> TempDir {
    let tmp = tempdir();
    let root = tmp.path();
    for d in 0..200 {
        let dir = root.join(format!("d{d:03}/sub"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            fs::write(dir.join(format!("f{f}")), vec![0u8; (f * 37) % 5000]).unwrap();
        }
    }
    tmp
}

fn scan(root: &std::path::Path, contend: bool) -> (dfdu_tree::Totals, Duration) {
    let e = Arc::new(Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 4,
            ..Default::default()
        },
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let reader = contend.then(|| {
        let (e, stop) = (e.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                e.read(|_| std::thread::sleep(Duration::from_millis(5)));
            }
        })
    });
    let t0 = Instant::now();
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(
        e.wait_idle(Some(Duration::from_secs(120))).is_idle(),
        "scan did not finish under contention"
    );
    let took = t0.elapsed();
    stop.store(true, Ordering::SeqCst);
    if let Some(r) = reader {
        r.join().unwrap();
    }
    let (t, timing) = e.read(|m| {
        (
            m.tree.total(m.tree.roots().next().unwrap()),
            m.status.timing.clone(),
        )
    });
    eprintln!(
        "contend {contend}: {} slices, lock wait p99 < {:?}, hold p99 < {:?}",
        timing.slices,
        Timing::percentile(&timing.wait, 0.99),
        Timing::percentile(&timing.hold, 0.99)
    );
    match Arc::try_unwrap(e) {
        Ok(e) => e.shutdown(Duration::from_secs(1)),
        Err(_) => unreachable!("reader joined"),
    }
    (t, took)
}

#[test]
fn slow_reader_does_not_change_or_stop_the_scan() {
    let tmp = tree();
    let (quiet, _) = scan(tmp.path(), false);
    let (busy, took) = scan(tmp.path(), true);
    assert_eq!(quiet, busy);
    assert_eq!(busy.items, 1 + 200 * 2 + 200 * 100);
    eprintln!("scan under read-lock contention took {took:?}");
}

/// How long the applier waits on its channels before it takes a slice with nothing to do.
const IDLE_WAIT: Duration = Duration::from_millis(50);

/// An engine with nothing to do waits on its channels instead of taking the write lock, also
/// with extent measurement off (macOS and Windows), where the measurers' channel is closed and
/// so always ready.
#[test]
fn idle_engine_does_not_spin() {
    for measure_workers in [0, 1] {
        let e = Engine::start(
            PortableBackend::default(),
            EngineOptions {
                workers: 1,
                measure_workers,
                journal_dir: None,
                crash_dir: None,
                ..Default::default()
            },
        );
        // Start-up slices are not counted: from here on, an idle applier takes the lock once
        // per `IDLE_WAIT`, however long the host keeps the test asleep.
        let before = e.read(|m| m.status.timing.slices);
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        let slices = e.read(|m| m.status.timing.slices) - before;
        let elapsed = t0.elapsed();
        let allowed = u64::try_from(elapsed.as_millis() / IDLE_WAIT.as_millis()).unwrap() + 3;
        assert!(
            slices <= allowed,
            "{slices} slices in {elapsed:?} with {measure_workers} measurers"
        );
        e.shutdown(Duration::from_secs(1));
    }
}
