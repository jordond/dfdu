//! A reader holding the read lock for long stretches (a slow renderer) delays the applier
//! between slices but cannot stop a scan or change its result (`docs/data-pipeline.md` §10.4).

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{JobOptions, PortableBackend};
use tempfile::TempDir;

/// A temporary directory under the canonical temp dir, so its path matches the scan root the
/// engine reports (on macOS the temp dir is behind the /var symlink).
fn tempdir() -> TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

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

fn scan(root: &std::path::Path, contend: bool) -> (dut_tree::Totals, Duration) {
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
        e.wait_idle(Some(Duration::from_secs(120))),
        "scan did not finish under contention"
    );
    let took = t0.elapsed();
    stop.store(true, Ordering::SeqCst);
    if let Some(r) = reader {
        r.join().unwrap();
    }
    let t = e.read(|m| m.tree.total(m.tree.roots().next().unwrap()));
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
