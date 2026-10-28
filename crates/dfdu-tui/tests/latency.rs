//! UI latency: time from start to the first frame that shows scanned data, and
//! from a key press to the frame that shows its effect, while a scan runs.
//!
//! This drives the real run loop (`dfdu_tui::event_loop`) and the real engine with the native
//! backend, as `dfdu` does after the terminal is set up. The loop reads keys from a channel this
//! harness writes to and draws to a `TestBackend` wrapped in a backend that timestamps every
//! flush. A key counts as shown at the first flush after the loop took it from the channel:
//! the loop takes one event, updates the state and redraws, all on its own thread, so the
//! first flush after the take is the frame that includes the key. Keys are sent on a schedule
//! (every 10-40 ms, not waiting for the previous frame), so a slow frame delays the keys
//! queued behind it and that delay is counted; a key is skipped while four are waiting.
//!
//! Start is taken before the storage probe and `Engine::start`, as in `dfdu_tui::run`; the
//! terminal setup and process start before it are not included.
//!
//! The smoke test runs in `cargo test` on a small tree with generous bounds. The measurement
//! on a large tree is `#[ignore]`d:
//!
//! ```text
//! cargo build --release -p dfdu-bench
//! target/release/dfdu-bench gen /tmp/tree-1m --entries 1000000 --seed 1
//! DFDU_LATENCY_TREE=/tmp/tree-1m DFDU_LATENCY_WORKERS=default,3,4 DFDU_LATENCY_RUNS=10 \
//!     cargo test --release -p dfdu-tui --test latency -- --ignored --nocapture
//! ```
//!
//! `DFDU_LATENCY_WORKERS` is a comma-separated list of worker counts (`default` is what `dfdu`
//! picks for the tree's storage); each is measured for `DFDU_LATENCY_RUNS` scans (default 10)
//! after one unmeasured scan that warms the cache. Check `uptime` before and after: other
//! processes on the machine change the numbers. `DFDU_LATENCY_VERBOSE=1` also prints every key
//! that took over 50 ms, with the times of the frames around it.
//!
//! `scripts/pty-latency.py` measures the real binary from outside, in a pseudo-terminal: start
//! of the process to the first data frame, and sort keys to frame.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::Recording;
use crossbeam_channel::{Sender, unbounded};
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::JobOptions;
use ratatui::Terminal;
use ratatui::buffer::Cell;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

/// Keys sent in turn: moves, open, back and sort changes.
const KEYS: &[char] = &[
    'j', 'j', 'l', 'j', 'k', 'h', 's', 's', 'j', 'n', 'n', 'l', 'h', 'C', 'M', 's', 'k',
];

/// Keys sent but not drawn yet beyond which no more are sent.
const MAX_BACKLOG: usize = 4;

#[derive(Copy, Clone)]
struct Sample {
    key: char,
    latency: Duration,
    /// Sent before the frame that shows the scan as done.
    running: bool,
    sent: Instant,
}

#[derive(Default)]
struct State {
    /// Keys sent so far.
    sent: u64,
    /// Keys not yet shown: (sequence number, key, time sent).
    pending: VecDeque<(u64, char, Instant)>,
    samples: Vec<Sample>,
    first_data: Option<Instant>,
    done: Option<Instant>,
    frames: u64,
    flushes: Vec<Instant>,
}

struct Probe {
    tx: Sender<Event>,
    state: Mutex<State>,
}

impl Probe {
    fn send(&self, c: char) {
        // Under the lock, so a flush sees the key counted in `sent` exactly when it is in the
        // channel or already taken.
        let mut s = self.state.lock().expect("probe");
        let seq = s.sent;
        s.sent += 1;
        s.pending.push_back((seq, c, Instant::now()));
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        self.tx.send(Event::Key(key)).expect("send key");
    }

    /// Called by the run loop's thread when a frame is flushed.
    fn flushed(&self, buf: &ratatui::buffer::Buffer) {
        let now = Instant::now();
        let mut s = self.state.lock().expect("probe");
        s.frames += 1;
        s.flushes.push(now);
        // Every key taken from the channel before this frame was drawn is in it.
        let taken = s.sent - self.tx.len() as u64;
        let running = s.done.is_none();
        while s.pending.front().is_some_and(|&(seq, ..)| seq < taken) {
            let (_, c, at) = s.pending.pop_front().expect("pending");
            s.samples.push(Sample {
                key: c,
                latency: now - at,
                running,
                sent: at,
            });
        }
        if s.first_data.is_none() && shows_data(buf) {
            s.first_data = Some(now);
        }
        if s.done.is_none() && row(buf, 0).contains("done in") {
            s.done = Some(now);
        }
    }
}

fn row(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf.cell((x, y)).map_or(" ", Cell::symbol))
        .collect()
}

/// The first list row is an entry, not "(empty)" or "(scanning…)".
fn shows_data(buf: &ratatui::buffer::Buffer) -> bool {
    let first = row(buf, 2);
    let first = first.trim();
    !first.is_empty() && !first.starts_with('(')
}

/// Deterministic jitter for key intervals (xorshift).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

struct Run {
    first_frame: Duration,
    scan: Duration,
    samples: Vec<Sample>,
    frames: u64,
    start: Instant,
    /// Upper bound of the applier's longest write-lock hold (a power of two).
    hold_max: Duration,
    flushes: Vec<Instant>,
}

/// Browses `tree` with `workers` scan workers (`None`: what `dfdu` picks) and sends keys from
/// the first data frame until the scan is done, then `after` more.
fn measure(tree: &Path, workers: Option<usize>, after: usize, seed: u64) -> Run {
    let (tx, rx) = unbounded();
    let probe = Arc::new(Probe {
        tx,
        state: Mutex::default(),
    });
    // Wide enough that a long tree path does not push "done in" off the header line.
    let flushed = probe.clone();
    let mut terminal =
        Terminal::new(Recording::new(200, 40, move |buf| flushed.flushed(buf))).expect("terminal");

    let start = Instant::now();
    let storage = dfdu_sys::storage(tree);
    let engine = Engine::start(
        dfdu_scan::native_backend(storage, false),
        EngineOptions {
            workers: workers.unwrap_or_else(|| dfdu_engine::workers_for(storage)),
            journal_dir: None,
            crash_dir: None,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: tree.to_path_buf(),
        opts: JobOptions::default(),
    });
    thread::scope(|scope| {
        let driver = probe.clone();
        scope.spawn(move || {
            let mut rng = Rng(seed | 1);
            let limit = start + Duration::from_secs(120);
            while driver.state.lock().expect("probe").first_data.is_none() {
                assert!(Instant::now() < limit, "no data frame");
                thread::sleep(Duration::from_millis(1));
            }
            let (mut i, mut extra) = (0, 0);
            let mut next = Instant::now();
            while Instant::now() < limit {
                next += Duration::from_millis(10 + rng.below(31));
                thread::sleep(next.saturating_duration_since(Instant::now()));
                let (done, backlog) = {
                    let s = driver.state.lock().expect("probe");
                    (s.done.is_some(), s.pending.len())
                };
                // A user would stop typing when nothing happens.
                if backlog >= MAX_BACKLOG {
                    continue;
                }
                if done {
                    if extra == after {
                        break;
                    }
                    extra += 1;
                }
                driver.send(KEYS[i % KEYS.len()]);
                i += 1;
            }
            // Let the last keys be drawn, then quit.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !driver.state.lock().expect("probe").pending.is_empty()
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(1));
            }
            // Keys never drawn count with the time they waited.
            let mut s = driver.state.lock().expect("probe");
            let now = Instant::now();
            while let Some((_, c, at)) = s.pending.pop_front() {
                s.samples.push(Sample {
                    key: c,
                    latency: now - at,
                    running: false,
                    sent: at,
                });
            }
            drop(s);
            driver.send('q');
        });
        dfdu_tui::event_loop(&mut terminal, &engine, rx).expect("browse");
    });
    let hold_max = engine.read(|m| dfdu_engine::Timing::max(&m.status.timing.hold));
    engine.shutdown(Duration::from_millis(200));
    let s = probe.state.lock().expect("probe");
    Run {
        first_frame: s.first_data.expect("data frame") - start,
        scan: s.done.map_or(Duration::MAX, |d| d - start),
        samples: s.samples.iter().filter(|x| x.key != 'q').copied().collect(),
        frames: s.frames,
        start,
        hold_max,
        flushes: s.flushes.clone(),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Nearest-rank percentile of sorted values.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a percentile's index: p is 0 to 100, so it lies between 0 and the length"
)]
fn pct(sorted: &[Duration], p: f64) -> Duration {
    let i = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[i.clamp(1, sorted.len()) - 1]
}

fn summary(v: &mut [Duration]) -> String {
    if v.is_empty() {
        return "n=0".into();
    }
    v.sort();
    format!(
        "n={:<4} p50 {:6.1} ms  p99 {:6.1} ms  max {:6.1} ms",
        v.len(),
        ms(pct(v, 50.0)),
        ms(pct(v, 99.0)),
        ms(*v.last().expect("max"))
    )
}

fn load() -> String {
    fs::read_to_string("/proc/loadavg").map_or_else(
        |_| "?".into(),
        |s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "),
    )
}

#[test]
#[ignore = "needs DFDU_LATENCY_TREE (a large dfdu-bench tree); see the top of this file"]
fn latency_large_tree() {
    let tree = std::env::var_os("DFDU_LATENCY_TREE")
        .expect("set DFDU_LATENCY_TREE to a directory, e.g. a dfdu-bench tree of 1M entries");
    let tree = Path::new(&tree);
    let runs: usize = std::env::var("DFDU_LATENCY_RUNS").map_or(10, |s| s.parse().expect("runs"));
    let workers = std::env::var("DFDU_LATENCY_WORKERS").unwrap_or_else(|_| "default".into());
    let verbose = std::env::var_os("DFDU_LATENCY_VERBOSE").is_some();
    let cpus = thread::available_parallelism().map_or(0, std::num::NonZero::get);
    println!(
        "tree {}, {cpus} CPUs, {runs} runs per setting",
        tree.display()
    );
    // Warms the cache.
    measure(tree, None, 0, 0);
    for w in workers.split(',') {
        let w = (w != "default").then(|| w.parse::<usize>().expect("worker count"));
        let name = w.map_or("default".to_string(), |n| n.to_string());
        let load_before = load();
        let (mut first, mut scan, mut all, mut during) = (vec![], vec![], vec![], vec![]);
        let mut by_key: Vec<(char, Vec<Duration>)> = vec![];
        let (mut frames, mut hold_max) = (0, Duration::ZERO);
        for r in 0..runs {
            let run = measure(tree, w, 0, r as u64 + 1);
            first.push(run.first_frame);
            scan.push(run.scan);
            frames += run.frames;
            hold_max = hold_max.max(run.hold_max);
            for x in &run.samples {
                all.push(x.latency);
                if x.running {
                    during.push(x.latency);
                }
                match by_key.iter_mut().find(|(k, _)| *k == x.key) {
                    Some((_, v)) => v.push(x.latency),
                    None => by_key.push((x.key, vec![x.latency])),
                }
                if verbose && x.latency > Duration::from_millis(50) {
                    println!(
                        "  run {r}: key {} sent at {:.0} ms (scan done at {:.0} ms) shown after {:.1} ms",
                        x.key,
                        ms(x.sent - run.start),
                        ms(run.scan),
                        ms(x.latency)
                    );
                    let near: Vec<String> = run
                        .flushes
                        .iter()
                        .filter(|&&f| {
                            f + Duration::from_millis(100) > x.sent
                                && f < x.sent + x.latency + Duration::from_millis(20)
                        })
                        .map(|&f| format!("{:.0}", ms(f - run.start)))
                        .collect();
                    println!("    frames at {}", near.join(" "));
                }
            }
        }
        println!(
            "\nworkers {name} (load before {load_before}, after {})",
            load()
        );
        println!("  first data frame  {}", summary(&mut first));
        println!("  scan (done shown) {}", summary(&mut scan));
        println!("  input to frame    {}", summary(&mut all));
        println!("    during the scan {}", summary(&mut during));
        by_key.sort_by_key(|(k, _)| *k);
        for (k, v) in &mut by_key {
            println!("    key {k}           {}", summary(v));
        }
        println!(
            "  frames drawn {frames}; longest applier write-lock hold under {:.1} ms",
            ms(hold_max)
        );
    }
}

/// The same measurement on a small tree, with bounds loose enough for slow CI machines.
#[test]
fn latency_smoke() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).expect("temp"))
        .expect("tempdir");
    for d in 0..40 {
        let dir = tmp.path().join(format!("d{d}/sub"));
        fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..50 {
            fs::write(dir.join(format!("f{f}")), vec![0u8; f * 100]).expect("write");
        }
    }
    let run = measure(tmp.path(), Some(2), 30, 7);
    let mut lat: Vec<Duration> = run.samples.iter().map(|s| s.latency).collect();
    println!("first data frame {:.1} ms", ms(run.first_frame));
    println!("input to frame {}", summary(&mut lat));
    assert!(
        run.first_frame < Duration::from_secs(5),
        "{:?}",
        run.first_frame
    );
    assert!(lat.len() >= 30, "every key is drawn: {} of 30", lat.len());
    assert!(
        *lat.last().expect("max") < Duration::from_secs(2),
        "{lat:?}"
    );
}
