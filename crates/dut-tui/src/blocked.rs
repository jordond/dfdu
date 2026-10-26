//! A UI blocked on terminal writes does not slow the scan, and data in transit stays within the
//! byte budget (ROADMAP M1, `docs/data-pipeline.md` §8 "Slow or blocked terminal").
//!
//! Runs the real event loop with a terminal whose writes block, against a scan of a tree on
//! disk, three ways: the loop drawing normally; the loop stuck in a terminal write for a fixed
//! time from before the scan starts (it built that frame under the read lock, released the lock,
//! and then blocked in `draw`); and, as a control, a reader that blocks while holding the read
//! lock, which a loop that drew under the lock would do. The first two must take about the same
//! time; the control must not finish before its block ends, which shows the test can see a UI
//! that stops the applier.
//!
//! In the blocked-in-draw run the backend holds one directory's open until the UI has blocked,
//! so the scan is running when the UI blocks whatever the machine's load, and it finishes while
//! the UI is still blocked.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{Backend, FsClass, JobOptions, NativeBackend};
use dut_tree::{Meta, Subvol, Totals};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

/// Writes pass while the gate is open and wait while it is closed.
#[derive(Default)]
struct Gate {
    closed: Mutex<bool>,
    opened: Condvar,
    /// A writer is waiting at the closed gate.
    waiting: AtomicBool,
    flushes: AtomicU64,
}

impl Gate {
    fn pass(&self) {
        let mut closed = self.closed.lock().unwrap();
        while *closed {
            self.waiting.store(true, SeqCst);
            closed = self.opened.wait(closed).unwrap();
        }
        self.waiting.store(false, SeqCst);
    }

    fn set(&self, closed: bool) {
        *self.closed.lock().unwrap() = closed;
        self.opened.notify_all();
    }
}

/// The terminal's output: discarded, after waiting at the gate.
struct GatedWriter(Arc<Gate>);

impl Write for GatedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.pass();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.pass();
        self.0.flushes.fetch_add(1, SeqCst);
        Ok(())
    }
}

/// Holds the scan until released.
#[derive(Default)]
struct Hold {
    released: Mutex<bool>,
    cv: Condvar,
}

impl Hold {
    /// Waits for `release`; gives up after a minute so a test that never releases fails instead
    /// of hanging.
    fn wait(&self) {
        let guard = self.released.lock().unwrap();
        let _ = self
            .cv
            .wait_timeout_while(guard, Duration::from_secs(60), |r| !*r)
            .unwrap();
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// The native backend, except that opening the second directory (the first below the root) waits
/// for `hold`. The other workers go on with the rest of the tree; the job cannot end until that
/// directory has been read.
struct Held {
    inner: NativeBackend,
    hold: Option<Arc<Hold>>,
    opens: AtomicUsize,
}

impl Backend for Held {
    type Loc = <NativeBackend as Backend>::Loc;
    type Stream = <NativeBackend as Backend>::Stream;

    fn loc_from_path(&self, path: &Path) -> Self::Loc {
        self.inner.loc_from_path(path)
    }
    fn stat_root(&self, loc: &Self::Loc) -> io::Result<Meta> {
        self.inner.stat_root(loc)
    }
    fn open(&self, loc: &Self::Loc) -> io::Result<Self::Stream> {
        if self.opens.fetch_add(1, SeqCst) == 1
            && let Some(h) = &self.hold
        {
            h.wait();
        }
        self.inner.open(loc)
    }
    fn subvolume(&self, loc: &Self::Loc) -> io::Result<Option<Subvol>> {
        self.inner.subvolume(loc)
    }
    fn shares_extents(&self, loc: &Self::Loc) -> bool {
        self.inner.shares_extents(loc)
    }
    fn data_profile(&self, loc: &Self::Loc) -> Option<(&'static str, f64)> {
        self.inner.data_profile(loc)
    }
    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        self.inner.duplicate_mounts(scope)
    }
    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        self.inner.mount_class(mount, dev)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ui {
    /// The real loop, drawing at its usual rate.
    Drawing,
    /// The real loop, blocked in a terminal write from before the scan starts until `block` has
    /// passed.
    BlockedInDraw,
    /// Control: a reader blocked for `block` while holding the read lock.
    BlockedHoldingLock,
}

struct Outcome {
    totals: Totals,
    took: Duration,
    /// How long the scan was held waiting for the UI to block; zero when it was not held.
    held: Duration,
    peak: usize,
    capacity: usize,
    frames: u64,
}

/// `dirs` directories of `dirs` subdirectories of `files` empty files each.
fn make_tree(root: &Path, dirs: usize, files: usize) {
    thread::scope(|s| {
        for t in 0..4 {
            s.spawn(move || {
                for a in (t..dirs).step_by(4) {
                    for b in 0..dirs {
                        let d = root.join(format!("a{a}/b{b}"));
                        fs::create_dir_all(&d).unwrap();
                        for f in 0..files {
                            fs::File::create(d.join(format!("f{f}"))).unwrap();
                        }
                    }
                }
            });
        }
    });
}

fn scan(root: &Path, ui: Ui, block: Duration) -> Outcome {
    let storage = dut_sys::storage(root);
    let hold = (ui == Ui::BlockedInDraw).then(|| Arc::new(Hold::default()));
    let engine = Arc::new(Engine::start(
        Held {
            inner: dut_scan::native_backend(storage),
            hold: hold.clone(),
            opens: AtomicUsize::new(0),
        },
        EngineOptions {
            workers: dut_engine::workers_for(storage),
            ..Default::default()
        },
    ));
    let gate = Arc::new(Gate::default());
    let (tx, input) = unbounded();
    let (go, go_rx) = unbounded::<()>();
    let (e, g) = (engine.clone(), gate.clone());
    let ui_thread = thread::spawn(move || match ui {
        Ui::Drawing | Ui::BlockedInDraw => {
            let backend = CrosstermBackend::new(GatedWriter(g));
            let viewport = Viewport::Fixed(Rect::new(0, 0, 120, 40));
            let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
            super::event_loop(&mut terminal, &*e, input)
        }
        Ui::BlockedHoldingLock => {
            let _ = go_rx.recv();
            e.read(|_| g.pass());
            Ok(())
        }
    });

    let t0 = Instant::now();
    engine.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    let mut held = Duration::ZERO;
    let release = (ui != Ui::Drawing).then(|| {
        // Block as soon as the job has started (the applier starts it under the write lock).
        while engine.read(|m| m.status.commands_applied) == 0 {
            thread::yield_now();
        }
        gate.set(true);
        if ui == Ui::BlockedInDraw {
            // Input makes the loop draw at once: it builds a frame under the read lock, then
            // blocks writing it.
            tx.send(Event::Resize(120, 40)).unwrap();
        } else {
            go.send(()).unwrap();
        }
        while !gate.waiting.load(SeqCst) {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "the UI never blocked"
            );
            thread::yield_now();
        }
        if ui == Ui::BlockedInDraw {
            // The held directory keeps the job from ending, so this holds by construction.
            assert!(
                engine.read(|m| m.status.busy()),
                "the scan ended before the UI blocked"
            );
            held = t0.elapsed();
            hold.as_ref().unwrap().release();
        }
        let g = gate.clone();
        thread::spawn(move || {
            thread::sleep(block);
            g.set(false);
        })
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(300))));
    let took = t0.elapsed();
    if ui == Ui::BlockedInDraw {
        assert!(
            *gate.closed.lock().unwrap() && gate.waiting.load(SeqCst),
            "the scan took {took:?}, longer than the UI was blocked ({block:?})"
        );
    }
    if let Some(r) = release {
        r.join().unwrap();
    }
    drop(tx);
    ui_thread.join().unwrap().unwrap();
    let totals = engine.read(|m| m.tree.total(m.tree.roots().next().unwrap()));
    let out = Outcome {
        totals,
        took,
        held,
        peak: engine.budget().peak(),
        capacity: engine.budget().capacity(),
        frames: gate.flushes.load(SeqCst),
    };
    match Arc::try_unwrap(engine) {
        Ok(e) => e.shutdown(Duration::from_secs(1)),
        Err(_) => unreachable!("the UI thread has ended"),
    }
    out
}

fn check(dirs: usize, files: usize, block: Duration) {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let t = Instant::now();
    make_tree(tmp.path(), dirs, files);
    let entries = 1 + dirs + dirs * dirs * (1 + files);
    eprintln!("tree of {entries} entries made in {:?}", t.elapsed());

    // Warm the cache, so the first measured scan is not the only cold one.
    scan(tmp.path(), Ui::Drawing, Duration::ZERO);
    let drawing = scan(tmp.path(), Ui::Drawing, Duration::ZERO);
    let blocked = scan(tmp.path(), Ui::BlockedInDraw, block);
    let control = scan(tmp.path(), Ui::BlockedHoldingLock, block);
    for (name, o) in [
        ("drawing", &drawing),
        ("blocked in draw", &blocked),
        ("blocked holding the read lock", &control),
    ] {
        eprintln!(
            "{name}: scan {:?} (held {:?}), {} flushes, in transit at most {:.1} MiB of {} MiB",
            o.took,
            o.held,
            o.frames,
            o.peak as f64 / (1 << 20) as f64,
            o.capacity >> 20
        );
        assert_eq!(o.totals.items, entries as u64, "{name}");
        assert!(o.peak <= o.capacity, "{name}");
    }
    assert_eq!(blocked.totals, drawing.totals);
    assert!(drawing.frames > 0);
    // The blocked run waited `held` for its UI to block before the held directory could be read.
    let allowed = drawing.took.mul_f64(1.5) + Duration::from_millis(100) + blocked.held;
    assert!(
        blocked.took <= allowed,
        "blocked UI: scan {:?}, drawing UI: {:?}",
        blocked.took,
        drawing.took
    );
    // A UI that held the read lock while blocked would stop the applier, and so the scan.
    assert!(control.took >= block, "control: {:?}", control.took);
}

#[test]
fn blocked_ui_does_not_slow_the_scan() {
    check(40, 20, Duration::from_secs(1));
}

/// About 1M entries and a UI blocked for 10 s. Run with
/// `cargo test --release -p dut-tui blocked_ui_10s -- --ignored --nocapture`.
#[test]
#[ignore]
fn blocked_ui_10s_on_a_million_entries() {
    check(100, 100, Duration::from_secs(10));
}
