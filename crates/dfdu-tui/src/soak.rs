//! Soak test: a scan of a large tree on disk with a
//! reader holding the read lock for 5 ms in a loop and the real UI loop drawing to a terminal
//! whose writes block for seconds at a time. The scan must complete with the totals of an
//! undisturbed scan, data in transit must stay within the byte budget, and the applier's p99
//! wait for the write lock must stay under 10 ms.
//!
//! The tree is not made here: 10M entries take minutes to create and gigabytes of disk. Make one
//! with `dfdu-bench gen` (`--entries 10000000`, and `--wide 10000000` for one directory of 10M
//! entries), then run
//! `DFDU_SOAK_TREE=/path cargo test --release -p dfdu-tui soak -- --ignored --nocapture`.
//! `DFDU_SOAK_BLOCK_MS` sets how long each terminal block lasts (default 3000); between blocks
//! the terminal accepts writes for 500 ms.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use dfdu_engine::{Command, Engine, EngineOptions, Timing};
use dfdu_scan::{JobOptions, NativeBackend};
use dfdu_tree::Totals;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::blocked::{Gate, GatedWriter};

fn engine(root: &Path) -> Engine<NativeBackend> {
    let storage = dfdu_sys::storage(root);
    Engine::start(
        dfdu_scan::native_backend(storage),
        EngineOptions {
            workers: dfdu_engine::workers_for(storage),
            ..Default::default()
        },
    )
}

fn root_totals(e: &Engine<NativeBackend>) -> Totals {
    e.read(|m| m.tree.total(m.tree.roots().next().unwrap()))
}

/// `VmHWM` and `VmRSS` of this process, in MiB (Linux; zero elsewhere).
fn memory() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0)
    };
    (field("VmHWM:"), field("VmRSS:"))
}

fn scan_quietly(root: &Path) -> (Totals, Duration) {
    let e = engine(root);
    let t0 = Instant::now();
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(
        e.wait_idle(Some(Duration::from_secs(3600))),
        "reference scan"
    );
    let took = t0.elapsed();
    let t = root_totals(&e);
    e.shutdown(Duration::from_secs(5));
    (t, took)
}

#[test]
#[ignore]
fn soak_busy_reader_and_blocked_terminal() {
    let Some(root) = std::env::var_os("DFDU_SOAK_TREE") else {
        eprintln!("DFDU_SOAK_TREE is not set; nothing to scan");
        return;
    };
    let root = dunce::canonicalize(root).unwrap();
    let block = Duration::from_millis(
        std::env::var("DFDU_SOAK_BLOCK_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000),
    );

    // Warm the cache, then take the totals of an undisturbed scan.
    scan_quietly(&root);
    let (reference, quiet) = scan_quietly(&root);
    eprintln!(
        "undisturbed: {} entries in {quiet:.2?}; process memory peak {:.0} MiB",
        reference.items,
        memory().0
    );

    let engine = Arc::new(engine(&root));
    let gate = Arc::new(Gate::default());
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, input) = unbounded();
    let (e, g) = (engine.clone(), gate.clone());
    let ui = thread::spawn(move || {
        let backend = CrosstermBackend::new(GatedWriter(g));
        let viewport = Viewport::Fixed(Rect::new(0, 0, 120, 40));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
        super::event_loop(&mut terminal, &*e, input)
    });
    let reads = Arc::new(AtomicU64::new(0));
    let (e, st, n) = (engine.clone(), stop.clone(), reads.clone());
    let reader = thread::spawn(move || {
        while !st.load(SeqCst) {
            e.read(|_| thread::sleep(Duration::from_millis(5)));
            n.fetch_add(1, SeqCst);
        }
    });
    // Blocks the terminal for `block`, opens it for 500 ms, and so on. Counts the blocks the UI
    // actually ran into, and the time it spent waiting in them.
    let (g, st) = (gate.clone(), stop.clone());
    let blocker = thread::spawn(move || {
        let (mut blocks, mut blocked) = (0u32, Duration::ZERO);
        while !st.load(SeqCst) {
            g.set(true);
            let t = Instant::now();
            let mut hit = None;
            while t.elapsed() < block && !st.load(SeqCst) {
                if hit.is_none() && g.waiting.load(SeqCst) {
                    hit = Some(Instant::now());
                }
                thread::sleep(Duration::from_millis(1));
            }
            if let Some(h) = hit {
                blocks += 1;
                blocked += h.elapsed();
            }
            g.set(false);
            thread::sleep(Duration::from_millis(500));
        }
        (blocks, blocked)
    });

    let t0 = Instant::now();
    engine.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    let done = engine.wait_idle(Some(Duration::from_secs(3600)));
    let took = t0.elapsed();
    let (peak_mem, rss) = memory();
    stop.store(true, SeqCst);
    let (blocks, blocked) = blocker.join().unwrap();
    reader.join().unwrap();
    gate.set(false);
    drop(tx);
    ui.join().unwrap().unwrap();
    assert!(done, "the scan did not finish within an hour");

    let totals = root_totals(&engine);
    let (timing, stats, fatal, incomplete, nodes) = engine.read(|m| {
        (
            m.status.timing.clone(),
            m.status.stats.clone(),
            m.status.fatal.clone(),
            m.status.incomplete,
            m.tree.live_nodes(),
        )
    });
    let (peak, capacity) = (engine.budget().peak(), engine.budget().capacity());
    let wait99 = Timing::percentile(&timing.wait, 0.99);
    let hold99 = Timing::percentile(&timing.hold, 0.99);
    let long = Timing::at_least(&timing.hold, Duration::from_millis(2));
    eprintln!(
        "soak: {} entries in {took:.2?} ({quiet:.2?} undisturbed); {} reads of 5 ms; terminal \
         blocked {blocks} times, {blocked:.1?} in all, {} frames written",
        totals.items,
        reads.load(SeqCst),
        gate.flushes.load(SeqCst),
    );
    eprintln!(
        "applier: {} slices, {:.2}M entries/s of lock time; wait p99 < {wait99:?}, max < {:?}; \
         hold p99 < {hold99:?}, {long} slices over 2 ms",
        timing.slices,
        stats.entries as f64 / (timing.locked_ns.max(1) as f64 / 1e9) / 1e6,
        Timing::max(&timing.wait),
    );
    eprintln!(
        "in transit at most {:.1} MiB of {} MiB; {nodes} nodes; memory: RSS {rss:.0} MiB at the \
         end, process peak {peak_mem:.0} MiB",
        peak as f64 / (1 << 20) as f64,
        capacity >> 20,
    );
    match Arc::try_unwrap(engine) {
        Ok(e) => e.shutdown(Duration::from_secs(5)),
        Err(_) => unreachable!("the UI and reader threads have ended"),
    }

    assert_eq!(fatal, None);
    assert!(!incomplete);
    assert_eq!(totals, reference);
    assert!(blocks > 0, "the UI never ran into a blocked terminal");
    assert!(peak <= capacity);
    // The percentile is a bucket's upper bound: 8.19 ms passes, 10.24 ms (8.19-10.24) does not.
    assert!(wait99 < Duration::from_millis(10), "p99 wait {wait99:?}");
}
