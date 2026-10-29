//! A wide directory read in many blocks, most of them stat'ed by chunk tasks on other workers, with
//! each backend: totals, items and subdirectories match a plain walk.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{Backend, DirStream, Forward, JobOptions, Limits, PortableBackend, Raw};
use dfdu_tree::Totals;

mod common;

/// Each block read takes 2 ms longer, so chunk tasks finish (and return the directory's credit)
/// while the listing still reads, on any OS. At full speed the listing can read the whole
/// directory before a woken worker finishes its first block: where stats are cheap (on Windows
/// the listing returns most of the metadata with the name), the listing worker stats its blocks
/// in about the time a woken thread takes to start, and the directory gets only the chunk tasks
/// of its initial credit (4).
struct SlowReads<B>(B);

struct SlowStream<S>(S);

impl<S: DirStream> DirStream for SlowStream<S> {
    type Loc = S::Loc;

    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<S::Loc>, bool)> {
        std::thread::sleep(Duration::from_millis(2));
        self.0.next_raw(max_entries, max_name_bytes)
    }
}

impl<B: Backend> Forward for SlowReads<B> {
    type Inner = B;
    type Stream = SlowStream<B::Stream>;

    fn inner(&self) -> &B {
        &self.0
    }
    fn open(&self, loc: &B::Loc, storage: dfdu_sys::Storage) -> io::Result<Self::Stream> {
        self.0.open(loc, storage).map(SlowStream)
    }
}

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
    let mut seen = HashSet::new();
    let (mut bytes, mut items) = (fs::metadata(root).unwrap().len(), 1);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            items += 1;
            if seen.insert(file_id(&e.path(), &m)) {
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
fn file_id(_path: &Path, m: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

/// The volume serial number and file id, which std reports only on nightly, from the file
/// opened for its attributes.
#[cfg(windows)]
fn file_id(path: &Path, _m: &fs::Metadata) -> (u64, u64) {
    let m = dfdu_tree::Meta::from_windows(&dfdu_sys::stat_path(path, false).unwrap());
    (m.dev, m.ino)
}

/// Without file ids: every entry is its own file. The fixture's hard link is then counted
/// twice by both sides.
#[cfg(not(any(unix, windows)))]
fn file_id(_path: &Path, _m: &fs::Metadata) -> (u64, u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
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
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(60))).is_idle());
    let out = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        assert_eq!(
            m.status.stats.corrupt + m.status.stats.lost + m.status.stats.duplicate,
            0
        );
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
    let tmp = common::tempdir();
    fixture(tmp.path());
    let (bytes, items) = walk(tmp.path());

    // At full speed, how many blocks go to chunk tasks depends on timing; the totals must match
    // whatever the split. With slowed reads nearly every block goes to a chunk task.
    let (t, _) = scan(PortableBackend::default(), tmp.path());
    assert_eq!((t.apparent, t.items, t.errors), (bytes, items, 0));
    let (n, chunk_tasks) = scan(SlowReads(PortableBackend::default()), tmp.path());
    assert_eq!(n, t, "portable backend, slowed reads");
    assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");

    #[cfg(target_os = "linux")]
    for storage in [dfdu_sys::Storage::Solid, dfdu_sys::Storage::Rotational] {
        let linux = || dfdu_scan::LinuxBackend::for_storage(storage);
        let (n, _) = scan(linux(), tmp.path());
        assert_eq!(
            n, t,
            "Linux backend ({storage:?}) and portable backend differ"
        );
        let (n, chunk_tasks) = scan(SlowReads(linux()), tmp.path());
        assert_eq!(n, t, "Linux backend ({storage:?}), slowed reads");
        assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");
    }

    #[cfg(target_os = "macos")]
    {
        let (n, _) = scan(dfdu_scan::MacBackend::default(), tmp.path());
        assert_eq!(n, t, "macOS backend and portable backend differ");
        let (n, chunk_tasks) = scan(SlowReads(dfdu_scan::MacBackend::default()), tmp.path());
        assert_eq!(n, t, "macOS backend, slowed reads");
        assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");
    }

    #[cfg(windows)]
    {
        let (n, _) = scan(dfdu_scan::WindowsBackend::default(), tmp.path());
        assert_eq!(n, t, "Windows backend and portable backend differ");
        let (n, chunk_tasks) = scan(SlowReads(dfdu_scan::WindowsBackend::default()), tmp.path());
        assert_eq!(n, t, "Windows backend, slowed reads");
        assert!(chunk_tasks > 10, "{chunk_tasks} chunk tasks");
    }
}
