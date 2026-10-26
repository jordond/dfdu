//! Applier throughput (D1, ROADMAP M2): records the scanner's messages for a directory once,
//! then times only the applier applying them, on one thread with nothing else running, so the
//! number does not depend on how the workers and the applier share the CPU.
//!
//! `cargo run --release -p dut-engine --example apply_rate -- DIR [RUNS]`

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dut_engine::{Applier, Command, Model};
use dut_scan::runtime::RealClock;
use dut_scan::{
    Batch, Clock, JobOptions, Limits, NativeBackend, ScanMsg, Shared, Sink, StepResult, Worker,
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

fn start(root: &Path) -> (Arc<Shared<NativeBackend>>, Applier<NativeBackend>, Model) {
    let shared = Arc::new(Shared::new(NativeBackend::default(), Limits::default()));
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
fn record(root: &Path) -> Vec<ScanMsg> {
    let (shared, mut applier, mut model) = start(root);
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

fn main() {
    let mut args = std::env::args().skip(1);
    let root = dunce::canonicalize(args.next().expect("usage: apply_rate DIR [RUNS]")).unwrap();
    let runs: usize = args.next().map_or(5, |r| r.parse().expect("RUNS"));
    let log = record(&root);
    let entries: u64 = log
        .iter()
        .map(|m| match m {
            ScanMsg::Chunk(c) => (c.files.len() + c.subdirs.len() + c.errors.len()) as u64,
            _ => 0,
        })
        .sum();
    let mut best = Duration::MAX;
    for _ in 0..runs {
        let (_shared, mut applier, mut model) = start(&root);
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
        "best: {:.2}M entries/s ({} messages)",
        entries as f64 / best.as_secs_f64() / 1e6,
        log.len()
    );
}
