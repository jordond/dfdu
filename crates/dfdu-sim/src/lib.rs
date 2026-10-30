//! Deterministic simulation of the scan pipeline.
//!
//! An in-memory filesystem with injected faults is scanned by the real worker, watchdog and
//! applier code, driven in a seeded random order on one thread with virtual time. After every
//! applier step the invariants are checked; at the end every directory's totals are compared
//! with a reference computed directly from the filesystem spec.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use dfdu_clean::{DeleterMsg, Plan};
use dfdu_engine::{Applier, CheckReq, Command, MeasureReq, Model};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{
    Backend, Budget, Clock, JobOptions, Limits, ScanMsg, Shared, Sink, StepResult, Worker,
};
use dfdu_sys::Storage;
use parking_lot::Mutex;

mod backend;
mod check;
mod config;
mod deleter;
mod end;
mod error;
mod random;
mod resume;
mod spec;
#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;

use backend::mount_key;
pub use backend::{SimBackend, SimClock, SimSink, SimStream};
pub use check::sim_check;
pub use config::{SimConfig, SimReport, sim_rules};
pub use error::SimError;
pub use resume::run_resumed;
pub use spec::{
    CACHEDIR_TAG, Fault, ROOT, ROOT_NAME, Rng, SimPiece, Spec, SpecBuilder, SpecConfig, SpecNode,
};

/// A simulation in progress. [`run`] drives one in random order from start to end; scenario
/// tests build their own spec, force the sequence they are
/// about with the step methods, then call [`Sim::run_to_end`], which finishes in random order
/// and applies every final check.
pub struct Sim {
    pub seed: u64,
    pub rng: Rng,
    cfg: SimConfig,
    pub spec: Arc<Spec>,
    pub clock: Arc<SimClock>,
    pub shared: Arc<Shared<SimBackend>>,
    pub sink: Arc<SimSink>,
    pub workers: Vec<Worker<SimBackend>>,
    pub applier: Applier<SimBackend>,
    pub model: Model,
    /// The batch being applied, with its budget bytes.
    cur: Option<(VecDeque<ScanMsg>, usize)>,
    rescans_left: u32,
    deletes_left: u32,
    cancels_left: u32,
    recoveries: u32,
    plans: VecDeque<(Plan, usize)>,
    del_q: VecDeque<DeleterMsg>,
    meas_q: Vec<MeasureReq>,
    check_q: Vec<CheckReq>,
    pub cross_mounts: bool,
    pub network: bool,
    pub report: SimReport,
    /// Parts `(worker, job, dir, part)` whose mount permit the watchdog took back.
    revoked: HashSet<(usize, u32, u32, u32)>,
    /// Every scan message applied, in order, when [`Sim::record`] is set (scenario tests).
    pub log: Vec<ScanMsg>,
    pub record: bool,
}

/// Keeps the default panic hook from printing the panics injected by [`Fault`]s; other panics
/// still print.
pub fn quiet_injected_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload().downcast_ref::<&str>().copied().unwrap_or("");
        if msg != "injected panic" {
            default(info);
        }
    }));
}

/// Runs one seed in random order from start to end.
///
/// # Errors
///
/// When a check of the run fails; the message names the seed and what went wrong.
pub fn run(seed: u64, cfg: &SimConfig) -> Result<SimReport, SimError> {
    Sim::new(seed, cfg).run_to_end()
}

impl Sim {
    /// A simulation of a spec generated from the seed, with the initial scan issued.
    #[must_use]
    pub fn new(seed: u64, cfg: &SimConfig) -> Sim {
        let mut rng = Rng::new(seed);
        let spec = Spec::generate(&mut rng, &cfg.spec, cfg.limits.block_entries);
        Sim::start(seed, rng, cfg, Arc::new(spec))
    }

    /// A simulation of a spec built by the caller, with the initial scan issued.
    #[must_use]
    pub fn with_spec(seed: u64, cfg: &SimConfig, spec: Spec) -> Sim {
        Sim::start(seed, Rng::new(seed), cfg, Arc::new(spec))
    }

    fn start(seed: u64, rng: Rng, cfg: &SimConfig, spec: Arc<Spec>) -> Sim {
        let mut sim = Sim::build(seed, rng, cfg, spec);
        let opts = sim.scan_opts();
        sim.applier.apply_command(
            &mut sim.model,
            Command::Scan {
                path: PathBuf::from(ROOT_NAME).into(),
                opts,
            },
        );
        sim
    }

    /// A simulation of `spec` with nothing issued yet.
    fn build(seed: u64, mut rng: Rng, cfg: &SimConfig, spec: Arc<Spec>) -> Sim {
        let cross_mounts = cfg.cross_mounts.unwrap_or_else(|| rng.chance(400));
        let network = cfg.network.unwrap_or_else(|| rng.chance(300));
        let (rotational, net) = cfg
            .mount_limits
            .unwrap_or_else(|| (1 + rng.index(2), 1 + rng.index(3)));
        let clock = Arc::new(SimClock(AtomicU64::new(1)));
        let backend = SimBackend {
            spec: spec.clone(),
            clock: clock.clone(),
            wrong_storage: AtomicU64::new(0),
        };
        let limits = Limits {
            pool_workers: cfg.workers,
            rotational_parts: rotational,
            network_parts: net,
            ..cfg.limits.clone()
        };
        let shared = Arc::new(Shared::new(backend, limits));
        let sink = Arc::new(SimSink {
            q: Mutex::new(VecDeque::new()),
            budget: Budget::new(cfg.budget),
            max_used: AtomicU64::new(0),
        });
        let workers = (0..cfg.workers)
            .map(|_| Worker::new(&shared, false))
            .collect();
        let mut applier = Applier::new(shared.clone());
        applier.checks = cfg.checks;
        Sim {
            seed,
            rng,
            cfg: cfg.clone(),
            spec,
            clock,
            shared,
            sink,
            workers,
            applier,
            model: Model::default(),
            cur: None,
            rescans_left: cfg.rescans,
            deletes_left: cfg.deletes,
            cancels_left: cfg.cancels,
            recoveries: 0,
            plans: VecDeque::new(),
            del_q: VecDeque::new(),
            meas_q: Vec::new(),
            check_q: Vec::new(),
            cross_mounts,
            network,
            report: SimReport::default(),
            revoked: HashSet::new(),
            log: Vec::new(),
            record: false,
        }
    }

    /// The options the root is scanned with.
    fn scan_opts(&self) -> JobOptions {
        JobOptions {
            cross_mounts: self.cross_mounts,
            network: self.network,
            class: self.cfg.rules.clone().map(|rules| ClassOpts {
                rules,
                path: ROOT_NAME.as_bytes().to_vec(),
                inside: vec![],
                protection: dfdu_rules::protected::State::default(),
                chain: vec![],
            }),
            ..JobOptions::default()
        }
    }

    /// A check made as the run steps that did not hold.
    fn fail(&self, what: impl std::fmt::Display) -> SimError {
        SimError {
            seed: self.seed,
            ticks: Some(self.report.ticks),
            what: what.to_string(),
        }
    }

    fn check(&self, after: &str) -> Result<(), SimError> {
        let wrong = self.shared.backend.wrong_storage.load(SeqCst);
        if wrong != 0 {
            return Err(self.fail(format!(
                "{wrong} directories opened with the storage of another mount{after}"
            )));
        }
        self.applier
            .check(&self.model)
            .map_err(|e| self.fail(format!("invariant{after}: {e}")))
    }

    pub fn step_worker(&mut self, i: usize) -> StepResult {
        self.workers[i].step(&self.shared, &*self.sink, &*self.clock)
    }

    /// Mount limits, checked against what the workers run: every part on a limited
    /// mount holds a permit unless it is a job root or the watchdog took its permit back while
    /// it was stuck; the permits held, including tasks handed over with one, are the gate's
    /// count; the parts holding one never exceed the limit; and the limit is the storage's,
    /// or the pool's size for a job root's mount.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn check_mount_limits(&mut self) -> Result<(), SimError> {
        let gates: HashMap<(u64, u64), dfdu_scan::GateStats> = self
            .shared
            .gate_stats()
            .into_iter()
            .map(|g| ((g.mount, g.dev), g))
            .collect();
        let mut held: HashMap<(u64, u64), usize> = HashMap::new();
        // Revocations of parts that have ended no longer matter.
        let current: HashSet<_> = (0..self.workers.len())
            .filter_map(|i| {
                let (job, dir, part) = self.workers[i].slot.current()?;
                Some((i, job.0, dir.0, part))
            })
            .collect();
        self.revoked.retain(|p| current.contains(p));
        for (i, w) in self.workers.iter().enumerate() {
            if let Some(loc) = w.handoff_loc() {
                *held
                    .entry(mount_key(&self.spec.nodes[loc as usize].meta))
                    .or_default() += 1;
            }
            let Some((loc, root, permit)) = w.running_part() else {
                continue;
            };
            let key = mount_key(&self.spec.nodes[loc as usize].meta);
            if permit {
                *held.entry(key).or_default() += 1;
            } else if gates.contains_key(&key) && !root {
                let (job, dir, part) = w.slot.current().ok_or_else(|| {
                    self.fail(format!("worker {i} runs {loc} with no current part"))
                })?;
                if !self.revoked.contains(&(i, job.0, dir.0, part)) {
                    return Err(self.fail(format!(
                        "worker {i} runs {loc} on limited mount {key:?} without a permit"
                    )));
                }
            }
        }
        let limits = &self.shared.limits;
        for (key, g) in &gates {
            let n = held.get(key).copied().unwrap_or(0);
            if n != g.running || n > g.limit {
                return Err(self.fail(format!("mount {key:?}: {n} permits held, gate {g:?}")));
            }
            self.report.at_limit += u64::from(n == g.limit);
            let class = match self.shared.backend.mount_storage(key.0, key.1) {
                Storage::Rotational => limits.rotational_parts,
                Storage::Network => limits.network_parts,
                _ => 0,
            };
            if g.limit != class && g.limit != limits.pool_workers {
                return Err(self.fail(format!("mount {key:?}: limit {} for {class}", g.limit)));
            }
        }
        Ok(())
    }

    /// Flushes worker `i`'s outbox: its messages become deliverable, its tasks stealable.
    pub fn flush_worker(&mut self, i: usize) -> bool {
        self.workers[i].flush(&self.shared, &*self.sink)
    }

    /// One watchdog round, then 1 ms of virtual time. Adds a worker when the runtime would
    /// replace stuck ones.
    ///
    /// # Panics
    ///
    /// If a worker running a part has no current part, a bug in the scan.
    #[expect(
        clippy::expect_used,
        reason = "a worker sets its current part before it runs one"
    )]
    pub fn watchdog(&mut self) {
        let before: Vec<bool> = self
            .workers
            .iter()
            .map(|w| w.running_part().is_some_and(|p| p.2))
            .collect();
        let rep = self.shared.watchdog_round(&*self.sink, self.clock.now());
        for (i, had) in before.into_iter().enumerate() {
            if had && self.workers[i].running_part().is_some_and(|p| !p.2) {
                let (job, dir, part) = self.workers[i].slot.current().expect("running");
                self.revoked.insert((i, job.0, dir.0, part));
            }
        }
        let target = self.cfg.workers + self.shared.mount_workers();
        if self.workers.len() < target + self.shared.allowed_extra_workers(self.cfg.workers, &rep) {
            self.workers.push(Worker::new(&self.shared, false));
        }
        self.report.max_workers = self.report.max_workers.max(self.workers.len());
        self.clock.advance(1_000_000);
    }

    pub fn advance(&self, ns: u64) {
        self.clock.advance(ns);
    }

    #[must_use]
    pub fn clock_now(&self) -> u64 {
        self.clock.now()
    }

    /// The applier's periodic tick, with the configured automatic abandon.
    pub fn tick(&mut self) {
        let now = self.clock.now();
        self.applier
            .tick(&mut self.model, now, Some(self.cfg.auto_abandon_ns));
    }

    /// Applies `cmd` and queues the delete plans it makes.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn command(&mut self, cmd: Command) -> Result<(), SimError> {
        self.applier.apply_command(&mut self.model, cmd);
        self.plans
            .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
        self.check(" after a command")
    }

    /// Messages delivered and not yet applied, in the order they will be applied.
    #[must_use]
    pub fn queued(&self) -> Vec<ScanMsg> {
        let mut out: Vec<ScanMsg> = self
            .cur
            .iter()
            .flat_map(|(m, _)| m.iter().cloned())
            .collect();
        for b in self.sink.q.lock().iter() {
            out.extend(b.msgs.iter().cloned());
        }
        out
    }

    /// Takes the first delivered message `pick` chooses out of the queue, so a script can
    /// deliver it later ([`Sim::deliver`]). Its batch's budget is released with the rest of it.
    pub fn take_queued(&mut self, pick: impl Fn(&ScanMsg) -> bool) -> Option<ScanMsg> {
        let mut q = self.sink.q.lock();
        for b in q.iter_mut() {
            if let Some(i) = b.msgs.iter().position(&pick) {
                return Some(b.msgs.remove(i));
            }
        }
        None
    }

    /// Queues `m` for the applier, as if a worker had sent it.
    pub fn deliver(&mut self, m: ScanMsg) {
        self.sink.send_control(m);
    }

    /// Applies the next delivered message. Returns it, or `None` when nothing is queued.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn apply_next(&mut self) -> Result<Option<ScanMsg>, SimError> {
        loop {
            if self.cur.is_none() {
                match self.sink.q.lock().pop_front() {
                    Some(b) => self.cur = Some((b.msgs.into(), b.bytes)),
                    None => return Ok(None),
                }
            }
            let Some((msgs, bytes)) = self.cur.as_mut() else {
                continue;
            };
            if let Some(m) = msgs.pop_front() {
                let now = self.clock.now();
                if self.record {
                    self.log.push(m.clone());
                }
                self.applier.apply(&mut self.model, m.clone(), now);
                self.applier.refresh_status(&mut self.model);
                self.check("")?;
                return Ok(Some(m));
            }
            self.sink.budget.release(*bytes);
            self.cur = None;
        }
    }

    /// Applies every delivered message.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn apply_all(&mut self) -> Result<(), SimError> {
        while self.apply_next()?.is_some() {}
        Ok(())
    }

    /// Runs the applier's own work (freeing, hard-link and extent passes) until none is left.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn applier_work(&mut self) -> Result<(), SimError> {
        while self.applier.has_work() {
            self.applier.work_step(&mut self.model);
            self.applier.refresh_status(&mut self.model);
            self.check("")?;
        }
        Ok(())
    }

    /// Takes the applier's measurement requests into the queue, one per file so answers can
    /// arrive in any order. A file is never asked about twice at once: a request for an inode
    /// whose earlier request in the same round is unanswered and still names a visible node
    /// fails the run.
    ///
    /// # Errors
    ///
    /// The duplicate request, with the seed.
    pub fn queue_measurements(&mut self) -> Result<(), SimError> {
        for r in self.applier.take_measure_requests() {
            for f in r.files {
                let tree = &self.model.tree;
                if self.meas_q.iter().any(|q| {
                    q.round == r.round
                        && q.files[0].inode == f.inode
                        && tree.resolve_visible(q.files[0].node).is_some()
                }) {
                    return Err(self.fail(format!(
                        "{} asked to be measured again before its answer",
                        r.dir.join(&f.name)
                    )));
                }
                self.meas_q.push(MeasureReq {
                    round: r.round,
                    dir: r.dir.clone(),
                    files: vec![f],
                });
            }
        }
        Ok(())
    }

    /// Answers every measurement asked for, and those the answers lead to; returns how many
    /// files were measured.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn answer_measurements(&mut self) -> Result<u64, SimError> {
        let mut measured = 0;
        loop {
            self.queue_measurements()?;
            let Some(req) = self.meas_q.pop() else {
                return Ok(measured);
            };
            let msg = self.shared.backend.measure(&req);
            measured += u64::from(matches!(
                msg.result,
                dfdu_engine::MeasureResult::Measured(_)
            ));
            let now = self.clock.now();
            self.applier.apply_measure(&mut self.model, msg, now);
            self.check(" after measurement")?;
        }
    }
}
