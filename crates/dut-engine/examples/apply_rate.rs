//! Applier throughput (D1, ROADMAP M2): records the scanner's messages for a tree once, then
//! times only the applier applying them, on one thread with nothing else running, so the
//! number does not depend on how the workers and the applier share the CPU.
//!
//! ```text
//! apply_rate [--runs N] DIR
//! apply_rate [--runs N] --synth ENTRIES [--wide N] [--seed S]
//! apply_rate [--runs N] --synth ENTRIES [--wide N] [--seed S] [--stat-ns NS] --engine WORKERS
//! ```
//!
//! `DIR` scans a tree on disk. `--synth` takes the tree `dut-bench gen` would create with the
//! same `--entries`, `--wide` and `--seed`, served from memory by `dut_bench::synth`, so a
//! 10M-entry run needs a few GB of RAM and no disk. `--engine` runs the real engine instead
//! (the applier thread and `WORKERS` worker threads) and prints the applier's lock times.
//! `--stat-ns` makes each synthetic entry cost that much CPU time in the worker that stats it,
//! as a real `statx` does; without it the workers produce entries far faster than any scan of
//! a disk, and every slice of the applier is full.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dut_bench::synth::{self, SynthBackend};
use dut_engine::{Applier, Command, Engine, EngineOptions, Model, Timing};
use dut_scan::runtime::RealClock;
use dut_scan::{
    Backend, Batch, Clock, JobOptions, Limits, NativeBackend, ScanMsg, Shared, Sink, StepResult,
    Worker,
};
use parking_lot::Mutex;

#[derive(Default)]
struct Collect(Mutex<VecDeque<Batch>>);

impl Sink for Collect {
    fn try_send(&self, b: Batch) -> Result<(), Batch> {
        self.0.lock().push_back(b);
        Ok(())
    }
    fn send(&self, b: Batch) {
        self.0.lock().push_back(b);
    }
    fn send_control(&self, m: ScanMsg) {
        self.0.lock().push_back(Batch {
            msgs: vec![m],
            bytes: 0,
        });
    }
}

fn start<B: Backend>(backend: B, root: &Path) -> (Arc<Shared<B>>, Applier<B>, Model) {
    let shared = Arc::new(Shared::new(backend, Limits::default()));
    let mut applier = Applier::new(shared.clone());
    let mut model = Model::default();
    applier.command(
        &mut model,
        Command::Scan {
            path: root.to_path_buf(),
            opts: JobOptions::default(),
        },
        1,
    );
    (shared, applier, model)
}

/// Applies everything until the job is done; returns the messages in the order applied.
fn record<B: Backend>(backend: B, root: &Path) -> Vec<ScanMsg> {
    let (shared, mut applier, mut model) = start(backend, root);
    let sink = Collect::default();
    let clock = RealClock::new();
    let mut worker = Worker::new(&shared, true);
    let mut log = vec![];
    loop {
        while worker.step(&shared, &sink, &clock) == StepResult::Progress {}
        shared.watchdog_round(&sink, clock.now());
        let batches: Vec<Batch> = sink.0.lock().drain(..).collect();
        for b in batches {
            for m in b.msgs {
                log.push(m.clone());
                applier.apply(&mut model, m, clock.now());
            }
        }
        while applier.has_work() {
            applier.work_step(&mut model);
        }
        applier.refresh_status(&mut model);
        if model.status.running() == 0 {
            return log;
        }
    }
}

fn applier_alone<B: Backend>(backend: impl Fn() -> B, root: &Path, runs: usize) {
    let t0 = Instant::now();
    let log = record(backend(), root);
    let entries: u64 = log
        .iter()
        .map(|m| match m {
            ScanMsg::Chunk(c) => (c.files.len() + c.subdirs.len() + c.errors.len()) as u64,
            _ => 0,
        })
        .sum();
    eprintln!(
        "recorded {} messages in {:.1} s",
        log.len(),
        t0.elapsed().as_secs_f64()
    );
    let mut best = Duration::MAX;
    for _ in 0..runs {
        let (_shared, mut applier, mut model) = start(backend(), root);
        let msgs = log.clone();
        let t0 = Instant::now();
        for m in msgs {
            applier.apply(&mut model, m, 2);
        }
        while applier.has_work() {
            applier.work_step(&mut model);
        }
        let took = t0.elapsed();
        assert_eq!(model.status.stats.entries, entries);
        best = best.min(took);
        eprintln!(
            "{entries} entries in {:.1} ms: {:.2}M entries/s",
            took.as_secs_f64() * 1e3,
            entries as f64 / took.as_secs_f64() / 1e6
        );
    }
    println!(
        "best: {:.2}M entries/s ({} messages){}",
        entries as f64 / best.as_secs_f64() / 1e6,
        log.len(),
        peak()
    );
}

/// The process's peak memory, where the system says (Linux).
fn peak() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse::<u64>().ok());
    kb.map_or(String::new(), |kb| {
        format!(", peak memory {} MiB", kb / 1024)
    })
}

/// The real engine with `workers` workers; prints what `dut scan --debug-stats` prints about
/// the applier.
fn engine<B: Backend>(backend: impl Fn() -> B, root: &Path, runs: usize, workers: usize) {
    for _ in 0..runs {
        let opts = EngineOptions {
            workers,
            journal_dir: None,
            crash_dir: None,
            read_only: true,
            measure_workers: 0,
            ..EngineOptions::default()
        };
        let t0 = Instant::now();
        let e = Engine::start(backend(), opts);
        e.send(Command::Scan {
            path: root.to_path_buf(),
            opts: JobOptions::default(),
        });
        assert!(e.wait_idle(None), "scan failed");
        let wall = t0.elapsed();
        e.read(|m| {
            let s = &m.status.stats;
            let t = &m.status.timing;
            let locked = Duration::from_nanos(t.locked_ns);
            let long = Timing::at_least(&t.hold, Duration::from_millis(2));
            println!(
                "{} entries in {:.2} s, {:.2}M entries/s of lock time; {} slices, hold p50 < \
                 {:?}, p99 < {:?}, max < {:?}, {long} ({:.2}%) over 2 ms; wait p99 < {:?}",
                s.entries,
                wall.as_secs_f64(),
                s.entries as f64 / locked.as_secs_f64().max(1e-9) / 1e6,
                t.slices,
                Timing::percentile(&t.hold, 0.5),
                Timing::percentile(&t.hold, 0.99),
                Timing::max(&t.hold),
                long as f64 * 100.0 / t.slices.max(1) as f64,
                Timing::percentile(&t.wait, 0.99),
            );
        });
        e.shutdown(Duration::from_secs(5));
    }
    println!("{}", peak().trim_start_matches(", "));
}

fn main() {
    let usage = "usage: apply_rate [--runs N] (DIR | --synth ENTRIES [--wide N] [--seed S] \
                 [--stat-ns NS]) [--engine WORKERS]";
    let mut args = std::env::args().skip(1);
    let (mut runs, mut synth, mut wide, mut seed, mut stat_ns, mut workers, mut dir) =
        (5, None, 0, 1, 0, None, None);
    let num = |v: Option<String>| -> u64 { v.and_then(|v| v.parse().ok()).expect(usage) };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = num(args.next()) as usize,
            "--synth" => synth = Some(num(args.next())),
            "--wide" => wide = num(args.next()),
            "--seed" => seed = num(args.next()),
            "--stat-ns" => stat_ns = num(args.next()),
            "--engine" => workers = Some(num(args.next()) as usize),
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            // The earlier form, `apply_rate DIR RUNS`.
            _ if dir.is_some() && a.parse::<usize>().is_ok() => runs = num(Some(a)) as usize,
            _ => panic!("{usage}"),
        }
    }
    match (synth, dir) {
        (Some(entries), None) => {
            let t0 = Instant::now();
            let b = SynthBackend::generate(entries, seed, wide).with_stat_ns(stat_ns);
            eprintln!(
                "planned {} entries (seed {seed}, wide {wide}) in {:.1} s",
                b.0.entries(),
                t0.elapsed().as_secs_f64()
            );
            let root = Path::new(synth::ROOT);
            match workers {
                Some(w) => engine(|| b.clone(), root, runs, w),
                None => applier_alone(|| b.clone(), root, runs),
            }
        }
        (None, Some(dir)) => {
            let root = dunce::canonicalize(dir).expect("DIR");
            match workers {
                Some(w) => engine(NativeBackend::default, &root, runs, w),
                None => applier_alone(NativeBackend::default, &root, runs),
            }
        }
        _ => panic!("{usage}"),
    }
}
