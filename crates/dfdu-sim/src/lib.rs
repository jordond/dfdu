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

use dfdu_clean::{DeleterMsg, Plan, Refusal};
use dfdu_engine::{
    Applier, CheckReq, Command, DeleteResult, MeasureReq, MeasureResult, Mode, Model,
};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{
    Backend, Budget, Clock, JobOptions, Limits, ScanMsg, Shared, Sink, StepResult, Worker,
};
use dfdu_sys::Storage;
use dfdu_tree::{ErrKind, Kind, NodeFlags, Tree};
use parking_lot::Mutex;

mod backend;
mod check;
mod spec;

use backend::mount_key;
pub use backend::{SimBackend, SimClock, SimSink, SimStream};
pub use check::sim_check;
use check::{check_classes, check_pool_flags, compare, random_dir, topmost_incomplete};
pub use spec::{
    CACHEDIR_TAG, Fault, ROOT, ROOT_NAME, Rng, SimPiece, Spec, SpecBuilder, SpecConfig, SpecNode,
};

#[derive(Clone)]
pub struct SimConfig {
    pub workers: usize,
    pub budget: usize,
    pub limits: Limits,
    pub auto_abandon_ns: u64,
    pub rescans: u32,
    /// Delete commands issued at random points; results are chosen by the simulated deleter.
    pub deletes: u32,
    /// Cancel commands for random running jobs.
    pub cancels: u32,
    pub max_ticks: u64,
    pub spec: SpecConfig,
    /// Scan with `cross_mounts`; `None` picks it per seed.
    pub cross_mounts: Option<bool>,
    /// Scan with `network`; `None` picks it per seed.
    pub network: Option<bool>,
    /// `Limits::rotational_parts` and `Limits::network_parts`; `None` picks them per seed
    /// (1-2 and 1-3).
    pub mount_limits: Option<(usize, usize)>,
    /// Classify against these rules, and check every class against a reference at the end.
    pub rules: Option<Arc<RuleSet>>,
    /// Runs the classifier pool: its requests are answered at random points, with answers
    /// that depend on the node's path ([`sim_check`]), and checked against them at the end.
    pub checks: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            workers: 3,
            budget: 16 * 1024,
            limits: Limits {
                block_entries: 3,
                block_name_bytes: 1 << 20,
                outbox_bytes: 4 * 1024,
                outbox_msgs: 5,
                flush_age_ns: 2_000_000,
                stall_ns: 10_000_000,
                chunk_credit: 2,
                ..Limits::default()
            },
            auto_abandon_ns: 400_000_000,
            rescans: 3,
            deletes: 3,
            cancels: 1,
            max_ticks: 2_000_000,
            spec: SpecConfig::default(),
            cross_mounts: None,
            network: None,
            mount_limits: None,
            rules: None,
            checks: false,
        }
    }
}

/// The built-in rules for a Linux home at `/home/u`, so that nothing below [`ROOT_NAME`] is a
/// protected or a known path, and only rules matched by name apply; and a rule for empty
/// directories anywhere below it, decided when their listings end.
///
/// # Panics
///
/// If the built-in rules or the empty-directory rule do not load, a bug in them.
#[must_use]
pub fn sim_rules() -> Arc<RuleSet> {
    const EMPTY: &str = r#"schema_version = 1
[[rule]]
id = "sim.empty"
category = "content.empty"
name = "Empty"
safety = "review"
priority = -10
[rule.match]
kind = "dir"
paths = ["/sim/**"]
empty = true
"#;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let (set, errors) = dfdu_rules::load_from(
        &env,
        &[
            (
                dfdu_rules::Source::Builtin,
                dfdu_rules::load::BUILTIN.into(),
            ),
            (dfdu_rules::Source::File("sim.toml".into()), EMPTY.into()),
        ],
    );
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(set)
}

#[derive(Debug, Default)]
pub struct SimReport {
    pub ticks: u64,
    /// Nodes the reference classifier gave a class to, all checked.
    pub classified: u64,
    /// Classifier pool answers applied, and classes in the final tree flagged git-tracked or
    /// in use by them.
    pub pool_answers: u64,
    pub tracked: u64,
    pub in_use: u64,
    pub dirs: usize,
    pub rescans: u32,
    pub deletes: u32,
    pub deleted_items: u64,
    pub cancels: u32,
    pub recoveries: u32,
    pub abandoned: u64,
    pub max_workers: usize,
    pub max_budget_used: u64,
    pub stalled_msgs: u64,
    pub late: u64,
    pub discarded_cut: u64,
    /// Subvolume records, read-only ones among them, and duplicate mounts in the final tree.
    pub subvols: u64,
    pub ro_subvols: u64,
    pub dup_mounts: u64,
    /// Runs with `cross_mounts`.
    pub cross_runs: u64,
    /// Network and kernel mounts left unscanned in the final tree.
    pub network_skipped: u64,
    pub kernel_skipped: u64,
    /// Measurements answered, and bytes counted once instead of per file in the final tree.
    pub measured: u64,
    pub shared_bytes: u64,
    /// Blocks stat'ed by chunk tasks rather than by their directory's listing task.
    pub chunk_tasks: u64,
    /// Mount limits created, tasks that waited for a permit, permits taken back from stuck
    /// parts, and checks that found a mount running at its limit.
    pub gates: u64,
    pub deferred: u64,
    pub revoked: u64,
    pub at_limit: u64,
}

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
pub fn run(seed: u64, cfg: &SimConfig) -> Result<SimReport, String> {
    Sim::new(seed, cfg).run_to_end()
}

impl Sim {
    /// A simulation of a spec generated from the seed, with the initial scan issued.
    #[must_use]
    pub fn new(seed: u64, cfg: &SimConfig) -> Sim {
        let mut rng = Rng::new(seed);
        let spec = Spec::generate(&mut rng, &cfg.spec, cfg.limits.block_entries);
        Sim::start(seed, rng, cfg, spec)
    }

    /// A simulation of a spec built by the caller, with the initial scan issued.
    #[must_use]
    pub fn with_spec(seed: u64, cfg: &SimConfig, spec: Spec) -> Sim {
        Sim::start(seed, Rng::new(seed), cfg, spec)
    }

    fn start(seed: u64, mut rng: Rng, cfg: &SimConfig, spec: Spec) -> Sim {
        let spec = Arc::new(spec);
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
        let mut sim = Sim {
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
        };
        sim.applier.apply_command(
            &mut sim.model,
            Command::Scan {
                path: PathBuf::from(ROOT_NAME),
                opts: JobOptions {
                    cross_mounts,
                    network,
                    class: cfg.rules.clone().map(|rules| ClassOpts {
                        rules,
                        path: ROOT_NAME.as_bytes().to_vec(),
                        inside: vec![],
                        protection: dfdu_rules::protected::State::default(),
                        chain: vec![],
                    }),
                    ..JobOptions::default()
                },
            },
        );
        sim
    }

    fn fail(&self, what: impl std::fmt::Display) -> String {
        format!(
            "seed {}: {what} (after {} ticks)",
            self.seed, self.report.ticks
        )
    }

    fn check(&self, after: &str) -> Result<(), String> {
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
    pub fn check_mount_limits(&mut self) -> Result<(), String> {
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
    pub fn command(&mut self, cmd: Command) -> Result<(), String> {
        self.applier.apply_command(&mut self.model, cmd);
        self.plans
            .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
        self.check(" after a command")
    }

    /// Paths of the items of the delete plan the deleter is running, if any.
    #[must_use]
    pub fn running_plan(&self) -> Option<Vec<PathBuf>> {
        self.plans
            .front()
            .map(|(p, _)| p.items.iter().map(|i| i.path.clone()).collect())
    }

    /// The deleter's result for the next item of the running plan, applied at once (a
    /// `Deleted` item is removed from the spec first). When no item is left, the plan's `Done`.
    /// Returns false if no plan is running.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn delete_next(&mut self, result: DeleteResult) -> Result<bool, String> {
        let Some((plan, i)) = self.plans.front_mut() else {
            return Ok(false);
        };
        let msg = if *i < plan.items.len() {
            let item = plan.items[*i].clone();
            *i += 1;
            if result == DeleteResult::Deleted {
                let n = self.shared.backend.loc_from_path(&item.path);
                self.spec.delete_subtree(n);
                self.report.deleted_items += 1;
            }
            DeleterMsg::Item {
                plan: plan.id,
                node: item.node,
                bytes: item.bytes,
                result,
            }
        } else {
            let id = plan.id;
            self.plans.pop_front();
            DeleterMsg::Done {
                plan: id,
                btrfs: false,
            }
        };
        self.applier.apply_delete(&mut self.model, msg);
        self.applier.refresh_status(&mut self.model);
        self.plans
            .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
        self.check(" after a delete result")?;
        Ok(true)
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
    pub fn apply_next(&mut self) -> Result<Option<ScanMsg>, String> {
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
    pub fn apply_all(&mut self) -> Result<(), String> {
        while self.apply_next()?.is_some() {}
        Ok(())
    }

    /// Runs the applier's own work (freeing, hard-link and extent passes) until none is left.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn applier_work(&mut self) -> Result<(), String> {
        while self.applier.has_work() {
            self.applier.work_step(&mut self.model);
            self.applier.refresh_status(&mut self.model);
            self.check("")?;
        }
        Ok(())
    }

    /// One step chosen at random: a worker step, answers to the applier's requests, applied
    /// messages and deleter results, a watchdog round, a tick, a deleter step or a command.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn random_step(&mut self) -> Result<(), String> {
        self.report.ticks += 1;
        self.check_q.extend(self.applier.take_check_requests());
        // One request per file, so answers can arrive in any order.
        for r in self.applier.take_measure_requests() {
            for f in r.files {
                self.meas_q.push(MeasureReq {
                    round: r.round,
                    dir: r.dir.clone(),
                    files: vec![f],
                });
            }
        }
        let r = self.rng.below(100);
        match r {
            0..55 => {
                let i = self.rng.index(self.workers.len());
                self.step_worker(i);
                self.check_mount_limits()?;
            }
            55..60 if !self.check_q.is_empty() && self.rng.chance(400) => {
                for _ in 0..=self.rng.below(4) {
                    if self.check_q.is_empty() {
                        break;
                    }
                    let i = self.rng.index(self.check_q.len());
                    let msg = sim_check(&self.check_q.swap_remove(i));
                    self.report.pool_answers += 1;
                    self.applier.apply_check(&mut self.model, msg);
                    self.check(" after a classifier answer")?;
                }
            }
            55..60 => {
                // Answers in any order, interleaved with everything else.
                for _ in 0..=self.rng.below(8) {
                    if self.meas_q.is_empty() {
                        break;
                    }
                    let i = self.rng.index(self.meas_q.len());
                    let req = self.meas_q.swap_remove(i);
                    let msg = self.shared.backend.measure(&req);
                    self.report.measured +=
                        u64::from(matches!(msg.result, MeasureResult::Measured(_)));
                    let now = self.clock.now();
                    self.applier.apply_measure(&mut self.model, msg, now);
                    self.check(" after measurement")?;
                }
            }
            60..80 => {
                for _ in 0..=self.rng.below(20) {
                    if let Some(d) = self.del_q.pop_front() {
                        if let (Some(tr), DeleterMsg::Item { node, result, .. }) = (trace(), &d) {
                            match self.model.tree.resolve(*node) {
                                Some(ix) => {
                                    let p = String::from_utf8_lossy(&self.model.tree.path(ix))
                                        .into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] result {p}: {result:?}", self.report.ticks);
                                    }
                                }
                                None => eprintln!(
                                    "[{}] result for unresolvable node: {result:?}",
                                    self.report.ticks
                                ),
                            }
                        }
                        self.applier.apply_delete(&mut self.model, d);
                        self.applier.refresh_status(&mut self.model);
                        self.check("")?;
                    } else if self.applier.has_work() {
                        self.applier.work_step(&mut self.model);
                        self.applier.refresh_status(&mut self.model);
                        self.check("")?;
                    } else if self.apply_next()?.is_none() {
                        break;
                    }
                }
                self.plans
                    .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
            }
            80..90 => {
                self.watchdog();
                self.check_mount_limits()?;
            }
            90..93 => self.tick(),
            93..96 => {
                if let Some(msg) = deleter_step(
                    &self.spec,
                    &self.shared,
                    &mut self.plans,
                    &mut self.rng,
                    &mut self.report,
                ) {
                    self.del_q.push_back(msg);
                }
            }
            _ => self.random_command()?,
        }
        self.check_q.extend(self.applier.take_check_requests());
        Ok(())
    }

    /// Time passes, and maybe a cancel, a delete or a rescan is issued.
    fn random_command(&mut self) -> Result<(), String> {
        self.clock.advance(self.rng.below(3) * 1_000_000);
        if self.cancels_left > 0 && self.rng.chance(8) {
            let running: Vec<_> = self
                .model
                .status
                .jobs
                .iter()
                .filter(|j| j.phase == dfdu_engine::JobPhase::Running)
                .map(|j| j.id)
                .collect();
            if !running.is_empty() {
                let job = running[self.rng.index(running.len())];
                self.cancels_left -= 1;
                self.report.cancels += 1;
                self.command(Command::Cancel { job })?;
            }
        }
        if self.deletes_left > 0 && self.rng.chance(15) {
            let items = pick_deletable(&self.model.tree, &self.spec, &self.shared, &mut self.rng);
            if !items.is_empty() {
                self.deletes_left -= 1;
                self.report.deletes += 1;
                if let Some(tr) = trace() {
                    for r in &items {
                        if let Some(ix) = self.model.tree.resolve(*r) {
                            let p = String::from_utf8_lossy(&self.model.tree.path(ix)).into_owned();
                            if tr.starts_with(&p) || p.starts_with(&tr) {
                                eprintln!("[{}] delete submitted {p}", self.report.ticks);
                            }
                        }
                    }
                }
                self.command(Command::Delete {
                    items,
                    mode: Mode::Permanent,
                })?;
            }
        }
        if self.rescans_left > 0
            && self.rng.chance(20)
            && let Some(node) = random_dir(&self.model.tree, &mut self.rng)
        {
            self.rescans_left -= 1;
            self.report.rescans += 1;
            let r = self.model.tree.node_ref(node);
            if let Some(tr) = trace() {
                let p = String::from_utf8_lossy(&self.model.tree.path(node)).into_owned();
                if tr.starts_with(&p) {
                    eprintln!("[{}] rescan requested {p}", self.report.ticks);
                }
            }
            self.command(Command::Rescan { node: r })?;
        }
        Ok(())
    }

    /// Nothing is running, queued or in flight anywhere.
    #[must_use]
    pub fn settled(&self) -> bool {
        let (out, ab) = self.shared.outstanding();
        out == ab
            && self.model.status.running() == 0
            && !self.applier.has_work()
            && self.cur.is_none()
            && self.sink.q.lock().is_empty()
            && self.model.status.commands_applied > 0
            && self.plans.is_empty()
            && self.del_q.is_empty()
            && self.model.status.deleting.is_none()
            && self.meas_q.is_empty()
            && self.model.status.measuring.is_none()
            && self.check_q.is_empty()
    }

    /// Steps in random order until everything has settled and no directory is left
    /// incomplete, then checks the final tree against the spec.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn run_to_end(mut self) -> Result<SimReport, String> {
        loop {
            if self.report.ticks >= self.cfg.max_ticks {
                let (out, ab) = self.shared.outstanding();
                eprintln!("{}", self.shared.debug_parts());
                return Err(self.fail(format!(
                    "did not finish: running jobs {}, outstanding {out}, abandoned {ab}, queued batches {}",
                    self.model.status.running(),
                    self.sink.q.lock().len()
                )));
            }
            self.random_step()?;
            if self.settled() {
                // A cancelled job leaves directories incomplete (Cancelled, or Superseded when
                // a cancelled rescan had cut them). Rescan the topmost one, as a user would,
                // until none remain; then the tree must match the filesystem again.
                match topmost_incomplete(&self.model.tree) {
                    Some(ix) if self.recoveries < 100 => {
                        self.recoveries += 1;
                        self.report.recoveries += 1;
                        let r = self.model.tree.node_ref(ix);
                        self.applier
                            .apply_command(&mut self.model, Command::Rescan { node: r });
                    }
                    Some(_) => {
                        return Err(self.fail("incomplete directories remain after 100 rescans"));
                    }
                    None => break,
                }
            }
        }
        self.finish()
    }

    fn finish(mut self) -> Result<SimReport, String> {
        let (spec, model) = (&self.spec, &self.model);
        let report = &mut self.report;
        report.chunk_tasks = self.shared.chunk_tasks.load(SeqCst);
        for g in self.shared.gate_stats() {
            report.gates += 1;
            report.deferred += g.deferred;
            report.revoked += g.revoked;
            // Parts still stuck forever lost their permits; nothing else is left running.
            if g.running != 0 || g.pending != 0 {
                return Err(format!(
                    "seed {}: mount limit left with work at the end: {g:?}",
                    self.seed
                ));
            }
        }
        report.subvols = model.subvols.len() as u64;
        report.ro_subvols = model.subvols.values().filter(|s| s.read_only).count() as u64;
        report.cross_runs = u64::from(self.cross_mounts);
        if let Some(root) = model.tree.roots().next() {
            let t = model.tree.total(root);
            report.shared_bytes = t.referenced.saturating_sub(t.alloc);
            let mut stack = vec![root];
            while let Some(ix) = stack.pop() {
                let f = model.tree.flags(ix);
                report.dup_mounts += u64::from(f.has(NodeFlags::DUP_MOUNT));
                report.network_skipped += u64::from(f.has(NodeFlags::NETWORK));
                report.kernel_skipped += u64::from(f.has(NodeFlags::KERNEL));
                stack.extend(model.tree.children(ix));
            }
        }
        let st = &model.status.stats;
        report.max_budget_used = self.sink.max_used.load(SeqCst);
        report.stalled_msgs = st.stalled;
        report.late = st.late;
        report.discarded_cut = st.discarded_cut;
        if st.corrupt + st.lost + st.duplicate > 0 {
            return Err(self.fail(format!(
                "corrupt {} lost {} duplicate {}: {:?}",
                st.corrupt,
                st.lost,
                st.duplicate,
                &model.status.anomalies[..model.status.anomalies.len().min(5)]
            )));
        }
        let used = report.max_budget_used;
        if used > self.cfg.budget as u64 {
            return Err(self.fail(format!("budget exceeded: {used}")));
        }
        let seed = self.seed;
        self.applier
            .check(model)
            .map_err(|e| format!("seed {seed}: final invariant: {e}"))?;
        self.applier
            .check_settled(model)
            .map_err(|e| format!("seed {seed}: settled: {e}"))?;
        if let Some(rules) = &self.cfg.rules {
            report.classified = check_classes(spec, model, rules)
                .map_err(|e| format!("seed {seed}: classes: {e}"))?;
            if self.cfg.checks {
                (report.tracked, report.in_use) = check_pool_flags(model, rules)
                    .map_err(|e| format!("seed {seed}: classifier pool: {e}"))?;
            }
        }
        if let Ok(v) = std::env::var("DFDU_SIM_INODE") {
            let mut it = v.split(',').map(str::parse::<u64>);
            let key = match (it.next(), it.next()) {
                (Some(Ok(dev)), Some(Ok(ino))) => (dev, ino),
                _ => return Err(format!("DFDU_SIM_INODE={v}: expected dev,ino")),
            };
            eprintln!("{}", self.applier.describe_extents(model, key));
            for n in spec
                .nodes
                .iter()
                .filter(|n| (n.meta.dev, n.meta.ino) == key)
            {
                eprintln!("  spec pieces {:?}", n.pieces);
            }
        }
        let (dirs, abandoned) = compare(
            spec,
            model,
            self.cfg.limits.block_entries,
            self.cross_mounts,
            self.network,
        )
        .map_err(|e| {
            let n = &model.status.notices;
            format!(
                "seed {seed}: {e}; last notices: {:?}",
                &n[n.len().saturating_sub(6)..]
            )
        })?;
        self.report.dirs = dirs;
        self.report.abandoned = abandoned;
        Ok(self.report)
    }
}

fn trace() -> Option<String> {
    std::env::var("DFDU_SIM_TRACE").ok()
}

/// One to three visible entries whose removal cannot shift a fault position.
fn pick_deletable(
    tree: &Tree,
    spec: &Spec,
    shared: &Shared<SimBackend>,
    rng: &mut Rng,
) -> Vec<dfdu_tree::NodeRef> {
    let Some(root) = tree.roots().next() else {
        return vec![];
    };
    let mut all = vec![];
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        stack.extend(tree.children(ix));
        if ix != root
            && !tree.flags(ix).has(NodeFlags::DELETING)
            && spec.deletable(shared.backend.loc_from_path(&tree.path_buf(ix)))
        {
            all.push(ix);
        }
    }
    let mut out = vec![];
    for _ in 0..=rng.below(3) {
        if all.is_empty() {
            break;
        }
        let i = rng.index(all.len());
        out.push(tree.node_ref(all.swap_remove(i)));
    }
    out
}

/// The simulated deleter: handles one item of the oldest plan, choosing a result and applying
/// it to the spec, or reports the plan done.
fn deleter_step(
    spec: &Spec,
    shared: &Shared<SimBackend>,
    plans: &mut VecDeque<(Plan, usize)>,
    rng: &mut Rng,
    report: &mut SimReport,
) -> Option<DeleterMsg> {
    let (plan, i) = plans.front_mut()?;
    if *i >= plan.items.len() {
        let id = plan.id;
        plans.pop_front();
        return Some(DeleterMsg::Done {
            plan: id,
            btrfs: false,
        });
    }
    let item = plan.items[*i].clone();
    *i += 1;
    let n = shared.backend.loc_from_path(&item.path);
    let is_dir = spec.nodes[n as usize].meta.kind == Kind::Dir;
    let result = match rng.below(100) {
        0..70 => {
            spec.delete_subtree(n);
            report.deleted_items += 1;
            DeleteResult::Deleted
        }
        70..78 => DeleteResult::Stale("changed".into()),
        78..82 => DeleteResult::Refused(Refusal::Protected),
        82..86 => DeleteResult::Aborted,
        86..92 => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
        _ if is_dir => {
            // Part of the tree went before the error.
            let mut stack: Vec<u32> = spec.live_children(n).collect();
            while let Some(d) = stack.pop() {
                stack.extend(spec.live_children(d));
                if spec.deletable(d) && rng.chance(500) {
                    spec.delete_subtree(d);
                }
            }
            DeleteResult::Failed {
                partial: true,
                err: ErrKind::Other,
                msg: "eio part-way".into(),
            }
        }
        _ => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
    };
    Some(DeleterMsg::Item {
        plan: plan.id,
        node: item.node,
        bytes: item.bytes,
        result,
    })
}
