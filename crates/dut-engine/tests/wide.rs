//! A wide directory read in many blocks, most of them stat'ed by chunk tasks on other workers
//! (`docs/data-pipeline.md` §4.2), with each backend: totals, items and subdirectories match a
//! plain walk.

use std::fs;
use std::path::Path;
use std::time::Duration;

use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{Backend, JobOptions, Limits, PortableBackend};
use dut_tree::Totals;

/// A directory of 3,000 files and 40 subdirectories (each with a file), plus a hard link pair.
fn fixture(root: &Path) {
    let wide = root.join("wide");
    fs::create_dir(&wide).unwrap();
    for i in 0..3000 {
        fs::write(wide.join(format!("f{i:04}")), vec![1u8; i % 9000]).unwrap();
        if i % 75 == 0 {
            let d = wide.join(format!("d{i:04}"));
            fs::create_dir(&d).unwrap();
            fs::write(d.join("inner"), vec![2u8; 5000]).unwrap();
        }
    }
    fs::hard_link(wide.join("f0100"), wide.join("link")).unwrap();
}

/// Apparent bytes and item count of everything below `root`, hard links once.
fn walk(root: &Path) -> (u64, u64) {
    let mut seen = std::collections::HashSet::new();
    let (mut bytes, mut items) = (fs::metadata(root).unwrap().len(), 1);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            items += 1;
            if seen.insert(file_id(&m)) {
                bytes += m.len();
            }
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    (bytes, items)
}

#[cfg(unix)]
fn file_id(m: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

/// Without file ids (Windows, until its backend): every entry is its own file. The fixture's
/// hard link is then counted twice by both sides.
#[cfg(not(unix))]
fn file_id(m: &fs::Metadata) -> (u64, u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let _ = m;
    (0, N.fetch_add(1, Ordering::Relaxed))
}

fn scan<B: Backend>(backend: B, root: &Path) -> (Totals, u64) {
    let e = Engine::start(
        backend,
        EngineOptions {
            check_invariants: true,
            workers: 4,
            // Small blocks: the 3,000-entry directory is read in about 50 of them.
            limits: Limits {
                block_entries: 64,
                ..Limits::default()
            },
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(60))));
    let out = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        assert!(m.status.stats.corrupt + m.status.stats.lost + m.status.stats.duplicate == 0);
        (m.tree.total(r), m.status.stats.chunks)
    });
    let chunk_tasks = e
        .scanner()
        .chunk_tasks
        .load(std::sync::atomic::Ordering::SeqCst);
    e.shutdown(Duration::from_secs(1));
    assert!(out.1 > 40, "the wide directory came in {} chunks", out.1);
    (out.0, chunk_tasks)
}

#[test]
fn wide_directory_in_chunk_tasks() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    fixture(tmp.path());
    let (bytes, items) = walk(tmp.path());

    let (t, chunk_tasks) = scan(PortableBackend::default(), tmp.path());
    assert_eq!((t.apparent, t.items, t.errors), (bytes, items, 0));
    assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");

    #[cfg(target_os = "linux")]
    for storage in [dut_sys::Storage::Solid, dut_sys::Storage::Rotational] {
        let (n, chunk_tasks) = scan(dut_scan::LinuxBackend::for_storage(storage), tmp.path());
        assert_eq!(
            n, t,
            "Linux backend ({storage:?}) and portable backend differ"
        );
        assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");
    }
}
