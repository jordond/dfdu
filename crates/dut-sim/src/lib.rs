//! Deterministic simulation of the scan pipeline (`docs/data-pipeline.md` §10.1).
//!
//! An in-memory filesystem with injected faults is scanned by the real worker, watchdog and
//! applier code, driven in a seeded random order on one thread with virtual time. After every
//! applier step the invariants are checked; at the end every directory's totals are compared
//! with a reference computed directly from the filesystem spec.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

use dut_clean::{DeleteResult, DeleterMsg, Mode, Plan, Refusal};
use dut_engine::{Applier, Command, Model};
use dut_scan::{
    Backend, Batch, Block, Budget, Clock, DirStream, JobOptions, Limits, ScanMsg, Shared, Sink,
    StepResult, Worker,
};
use dut_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, SEP, Totals, Tree};
use parking_lot::Mutex;

// ---- randomness ------------------------------------------------------------------------------

/// SplitMix64: small, fast, and identical on every platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
    /// True with probability `per_mille / 1000`.
    pub fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

// ---- filesystem spec -------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Opening the directory fails with permission denied.
    Eacces,
    /// Reading fails with EIO after this many entries.
    EioAfter(usize),
    /// Reading panics when it reaches this block.
    PanicAtBlock(usize),
    PanicOnOpen,
    /// Reading blocks when it reaches `block`, for `dur_ns` of virtual time, or forever.
    Stall {
        block: usize,
        dur_ns: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct SpecNode {
    pub name: Vec<u8>,
    pub meta: Meta,
    pub children: Vec<u32>,
    pub fault: Fault,
    /// The entry disappears between listing and stat (files only).
    pub vanished: bool,
}

pub struct Spec {
    pub nodes: Vec<SpecNode>,
    pub parent: Vec<u32>,
    /// Set when the simulated deleter removes the entry.
    pub deleted: Vec<AtomicBool>,
}

impl Spec {
    pub fn is_deleted(&self, i: u32) -> bool {
        self.deleted[i as usize].load(SeqCst)
    }

    pub fn delete_subtree(&self, i: u32) {
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            self.deleted[n as usize].store(true, SeqCst);
            stack.extend(&self.nodes[n as usize].children);
        }
    }

    /// Entries the simulated deleter may remove: their parent's listing has no fault, so
    /// removing them cannot shift a fault's position (see `run`).
    pub fn deletable(&self, i: u32) -> bool {
        i != ROOT
            && !self.is_deleted(i)
            && self.nodes[self.parent[i as usize] as usize].fault == Fault::None
    }

    pub fn live_children(&self, i: u32) -> impl Iterator<Item = u32> + '_ {
        self.nodes[i as usize]
            .children
            .iter()
            .copied()
            .filter(|&c| !self.is_deleted(c))
    }
}

pub const ROOT: u32 = 0;
pub const ROOT_NAME: &str = "/sim";

pub struct SpecConfig {
    pub max_depth: u32,
    pub max_dirs: u64,
    pub max_files: u64,
    pub faults: bool,
    pub hardlinks: bool,
    pub other_fs: bool,
}

impl Default for SpecConfig {
    fn default() -> Self {
        SpecConfig {
            max_depth: 4,
            max_dirs: 4,
            max_files: 7,
            faults: true,
            hardlinks: true,
            other_fs: true,
        }
    }
}

impl Spec {
    pub fn generate(rng: &mut Rng, cfg: &SpecConfig, block: usize) -> Spec {
        let mut ino = 1u64;
        let mut nodes = vec![SpecNode {
            name: ROOT_NAME.as_bytes().to_vec(),
            meta: Meta {
                kind: Kind::Dir,
                apparent: 4096,
                alloc: 4096,
                mtime: 1,
                dev: 1,
                ino,
                nlink: 2,
            },
            children: vec![],
            fault: Fault::None,
            vanished: false,
        }];
        let mut queue = VecDeque::from([(ROOT, 0u32)]);
        while let Some((d, depth)) = queue.pop_front() {
            let dev = nodes[d as usize].meta.dev;
            let ndirs = if depth < cfg.max_depth {
                rng.below(cfg.max_dirs + 1)
            } else {
                0
            };
            let nfiles = rng.below(cfg.max_files + 1);
            let mut kids = vec![];
            for i in 0..ndirs {
                ino += 1;
                let other = cfg.other_fs && depth > 0 && rng.chance(30);
                kids.push(nodes.len() as u32);
                nodes.push(SpecNode {
                    name: format!("d{i}").into_bytes(),
                    meta: Meta {
                        kind: Kind::Dir,
                        apparent: 4096,
                        alloc: 4096,
                        mtime: 1,
                        dev: if other { dev + 1 } else { dev },
                        ino,
                        nlink: 2,
                    },
                    children: vec![],
                    fault: Fault::None,
                    vanished: false,
                });
                queue.push_back((*kids.last().expect("pushed"), depth + 1));
            }
            for i in 0..nfiles {
                ino += 1;
                let size = rng.below(100_000);
                let alloc = if rng.chance(100) {
                    0
                } else {
                    size.div_ceil(4096) * 4096
                };
                kids.push(nodes.len() as u32);
                nodes.push(SpecNode {
                    name: format!("f{i}").into_bytes(),
                    meta: Meta {
                        kind: Kind::File,
                        apparent: size,
                        alloc,
                        mtime: 1,
                        dev,
                        ino,
                        nlink: 1,
                    },
                    children: vec![],
                    fault: Fault::None,
                    vanished: cfg.faults && rng.chance(30),
                });
            }
            nodes[d as usize].children = kids;
        }
        // Hard links: some files become extra links of another file's inode (same device).
        if cfg.hardlinks {
            let files: Vec<u32> = (0..nodes.len() as u32)
                .filter(|&i| {
                    nodes[i as usize].meta.kind == Kind::File && !nodes[i as usize].vanished
                })
                .collect();
            for _ in 0..files.len() / 4 {
                let a = files[rng.below(files.len() as u64) as usize] as usize;
                let b = files[rng.below(files.len() as u64) as usize] as usize;
                if a == b || nodes[a].meta.dev != nodes[b].meta.dev {
                    continue;
                }
                let m = nodes[a].meta;
                nodes[b].meta = Meta { nlink: 1, ..m };
            }
            let mut count: HashMap<(u64, u64), u32> = HashMap::new();
            for n in nodes.iter().filter(|n| n.meta.kind == Kind::File) {
                *count.entry((n.meta.dev, n.meta.ino)).or_default() += 1;
            }
            for n in nodes.iter_mut().filter(|n| n.meta.kind == Kind::File) {
                let c = count[&(n.meta.dev, n.meta.ino)];
                // Some inodes have links outside the scanned tree as well.
                n.meta.nlink = if c > 1 {
                    c + (n.meta.ino % 2) as u32
                } else {
                    1
                };
            }
        }
        if cfg.faults {
            for node in nodes.iter_mut().skip(1) {
                if node.meta.kind != Kind::Dir {
                    continue;
                }
                let len = node.children.len();
                let last_block = len.saturating_sub(1) / block;
                let r = rng.below(1000);
                node.fault = match r {
                    0..50 => Fault::Eacces,
                    50..90 => Fault::EioAfter(rng.below(len as u64 + 1) as usize),
                    90..110 if len > 0 => {
                        Fault::PanicAtBlock(rng.below(last_block as u64 + 1) as usize)
                    }
                    110..120 => Fault::PanicOnOpen,
                    120..150 => Fault::Stall {
                        block: rng.below(last_block as u64 + 1) as usize,
                        dur_ns: Some(20_000_000 + rng.below(200_000_000)),
                    },
                    150..170 => Fault::Stall {
                        block: rng.below(last_block as u64 + 1) as usize,
                        dur_ns: None,
                    },
                    _ => Fault::None,
                };
            }
        }
        let mut parent = vec![ROOT; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            for &c in &n.children {
                parent[c as usize] = i as u32;
            }
        }
        let deleted = (0..nodes.len()).map(|_| AtomicBool::new(false)).collect();
        Spec {
            nodes,
            parent,
            deleted,
        }
    }
}

// ---- backend, clock, sink --------------------------------------------------------------------

pub struct SimClock(AtomicU64);

impl SimClock {
    pub fn advance(&self, ns: u64) {
        self.0.fetch_add(ns, SeqCst);
    }
}

impl Clock for SimClock {
    fn now(&self) -> u64 {
        self.0.load(SeqCst)
    }
}

pub struct SimBackend {
    pub spec: Arc<Spec>,
    pub clock: Arc<SimClock>,
}

pub struct SimStream {
    spec: Arc<Spec>,
    clock: Arc<SimClock>,
    node: u32,
    /// Live children when the directory was opened.
    children: Vec<u32>,
    pos: usize,
    stall_start: u64,
    stall_done: bool,
    eio_pending: bool,
}

impl DirStream for SimStream {
    type Loc = u32;

    fn next_block(
        &mut self,
        out: &mut Block<u32>,
        max_entries: usize,
        _max_bytes: usize,
    ) -> io::Result<bool> {
        let n = &self.spec.nodes[self.node as usize];
        let block_idx = self.pos / max_entries;
        if self.eio_pending {
            return Err(io::Error::from_raw_os_error(5));
        }
        match n.fault {
            Fault::Stall { block, dur_ns } if block == block_idx && !self.stall_done => {
                let now = self.clock.now();
                if self.stall_start == 0 {
                    self.stall_start = now;
                }
                match dur_ns {
                    Some(d) if now >= self.stall_start + d => self.stall_done = true,
                    _ => return Err(io::ErrorKind::WouldBlock.into()),
                }
            }
            Fault::PanicAtBlock(b) if b == block_idx => panic!("injected panic"),
            _ => {}
        }
        let len = self.children.len();
        let limit = match n.fault {
            Fault::EioAfter(k) => k.min(len),
            _ => len,
        };
        if self.pos >= limit && limit < len {
            return Err(io::Error::from_raw_os_error(5));
        }
        let end = (self.pos + max_entries).min(limit);
        for &c in &self.children[self.pos..end] {
            let cn = &self.spec.nodes[c as usize];
            let meta = if cn.vanished {
                Err(ErrKind::NotFound)
            } else {
                Ok(cn.meta)
            };
            let child = (cn.meta.kind == Kind::Dir).then_some(c);
            out.push(&cn.name, meta, child);
        }
        self.pos = end;
        if self.pos >= limit && limit < len {
            self.eio_pending = true;
            return Ok(true);
        }
        Ok(self.pos < len)
    }
}

impl Backend for SimBackend {
    type Loc = u32;
    type Stream = SimStream;

    fn loc_from_path(&self, path: &Path) -> u32 {
        // Paths below the root are "/sim/d0/d1"; resolve them by name.
        let s = path.to_string_lossy();
        let rest = s.strip_prefix(ROOT_NAME).unwrap_or("");
        let mut cur = ROOT;
        for part in rest.split(['/', '\\']).filter(|p| !p.is_empty()) {
            let n = &self.spec.nodes[cur as usize];
            cur = *n
                .children
                .iter()
                .find(|&&c| self.spec.nodes[c as usize].name == part.as_bytes())
                .expect("path in spec");
        }
        cur
    }

    fn stat_root(&self, loc: &u32) -> io::Result<Meta> {
        Ok(self.spec.nodes[*loc as usize].meta)
    }

    fn open(&self, loc: &u32) -> io::Result<SimStream> {
        if self.spec.is_deleted(*loc) {
            return Err(io::ErrorKind::NotFound.into());
        }
        match self.spec.nodes[*loc as usize].fault {
            Fault::Eacces => return Err(io::ErrorKind::PermissionDenied.into()),
            Fault::PanicOnOpen => panic!("injected panic"),
            _ => {}
        }
        Ok(SimStream {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: *loc,
            children: self.spec.live_children(*loc).collect(),
            pos: 0,
            stall_start: 0,
            stall_done: false,
            eio_pending: false,
        })
    }
}

pub struct SimSink {
    q: Mutex<VecDeque<Batch>>,
    pub budget: Budget,
    pub max_used: AtomicU64,
}

impl Sink for SimSink {
    fn try_send(&self, mut b: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(b.bytes) {
            Some(n) => {
                b.bytes = n;
                self.q.lock().push_back(b);
                self.max_used.fetch_max(self.budget.used() as u64, SeqCst);
                Ok(())
            }
            None => Err(b),
        }
    }
    fn send(&self, _b: Batch) {
        unreachable!("simulated workers never block");
    }
    fn send_control(&self, m: ScanMsg) {
        self.q.lock().push_back(Batch {
            msgs: vec![m],
            bytes: 0,
        });
    }
}

// ---- driver ----------------------------------------------------------------------------------

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
            },
            auto_abandon_ns: 400_000_000,
            rescans: 3,
            deletes: 3,
            cancels: 1,
            max_ticks: 2_000_000,
            spec: SpecConfig::default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct SimReport {
    pub ticks: u64,
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
}

pub fn run(seed: u64, cfg: &SimConfig) -> Result<SimReport, String> {
    let mut rng = Rng::new(seed);
    let spec = Arc::new(Spec::generate(
        &mut rng,
        &cfg.spec,
        cfg.limits.block_entries,
    ));
    let clock = Arc::new(SimClock(AtomicU64::new(1)));
    let backend = SimBackend {
        spec: spec.clone(),
        clock: clock.clone(),
    };
    let shared = Arc::new(Shared::new(backend, cfg.limits.clone()));
    let sink = Arc::new(SimSink {
        q: Mutex::new(VecDeque::new()),
        budget: Budget::new(cfg.budget),
        max_used: AtomicU64::new(0),
    });
    let mut workers: Vec<Worker<SimBackend>> = (0..cfg.workers)
        .map(|_| Worker::new(&shared, false))
        .collect();
    let mut applier = Applier::new(shared.clone());
    let mut model = Model::default();
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut rescans_left = cfg.rescans;
    let mut deletes_left = cfg.deletes;
    let mut cancels_left = cfg.cancels;
    let mut recoveries = 0;
    let mut plans: VecDeque<(Plan, usize)> = VecDeque::new();
    let mut del_q: VecDeque<DeleterMsg> = VecDeque::new();
    let mut report = SimReport::default();

    applier.command(
        &mut model,
        Command::Scan {
            path: PathBuf::from(ROOT_NAME),
            opts: JobOptions::default(),
        },
        clock.now(),
    );
    let fail = |what: String, report: &SimReport| {
        Err(format!(
            "seed {seed}: {what} (after {} ticks)",
            report.ticks
        ))
    };

    loop {
        report.ticks += 1;
        if report.ticks > cfg.max_ticks {
            let (out, ab) = shared.outstanding();
            return fail(
                format!(
                    "did not finish: running jobs {}, outstanding {out}, abandoned {ab}, queued batches {}",
                    model.status.running(),
                    sink.q.lock().len()
                ),
                &report,
            );
        }
        let r = rng.below(100);
        match r {
            0..60 => {
                let i = rng.below(workers.len() as u64) as usize;
                let _: StepResult = workers[i].step(&shared, &*sink, &*clock);
            }
            60..80 => {
                for _ in 0..=rng.below(20) {
                    if let Some(d) = del_q.pop_front() {
                        if let (Some(tr), DeleterMsg::Item { node, result, .. }) = (trace(), &d) {
                            match model.tree.resolve(*node) {
                                Some(ix) => {
                                    let p =
                                        String::from_utf8_lossy(&model.tree.path(ix)).into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] result {p}: {result:?}", report.ticks);
                                    }
                                }
                                None => eprintln!(
                                    "[{}] result for unresolvable node: {result:?}",
                                    report.ticks
                                ),
                            }
                        }
                        applier.apply_delete(&mut model, d);
                    } else if applier.has_work() {
                        applier.work_step(&mut model);
                    } else {
                        if cur.is_none() {
                            match sink.q.lock().pop_front() {
                                Some(b) => cur = Some((b.msgs.into(), b.bytes)),
                                None => break,
                            }
                        }
                        let (msgs, bytes) = cur.as_mut().expect("batch");
                        match msgs.pop_front() {
                            Some(m) => applier.apply(&mut model, m, clock.now()),
                            None => {
                                sink.budget.release(*bytes);
                                cur = None;
                            }
                        }
                    }
                    applier.refresh_status(&mut model);
                    if let Err(e) = applier.check(&model) {
                        return fail(format!("invariant: {e}"), &report);
                    }
                }
                plans.extend(applier.take_plans().into_iter().map(|p| (p, 0)));
            }
            80..90 => {
                let rep = shared.watchdog_round(&*sink, clock.now());
                // Same replacement policy as the real runtime. Simulated workers never exit, so
                // the pool only grows here; that is the worst case for the bound.
                if workers.len() < cfg.workers + shared.allowed_extra_workers(cfg.workers, &rep) {
                    workers.push(Worker::new(&shared, false));
                }
                report.max_workers = report.max_workers.max(workers.len());
                clock.advance(1_000_000);
            }
            90..93 => applier.tick(&mut model, clock.now(), Some(cfg.auto_abandon_ns)),
            93..96 => {
                if let Some(msg) = deleter_step(&spec, &shared, &mut plans, &mut rng, &mut report) {
                    del_q.push_back(msg);
                }
            }
            _ => {
                clock.advance(rng.below(3) * 1_000_000);
                if cancels_left > 0 && rng.chance(8) {
                    let running: Vec<_> = model
                        .status
                        .jobs
                        .iter()
                        .filter(|j| j.phase == dut_engine::JobPhase::Running)
                        .map(|j| j.id)
                        .collect();
                    if !running.is_empty() {
                        let job = running[rng.below(running.len() as u64) as usize];
                        cancels_left -= 1;
                        report.cancels += 1;
                        applier.command(&mut model, Command::Cancel { job }, clock.now());
                        if let Err(e) = applier.check(&model) {
                            return fail(format!("invariant after cancel: {e}"), &report);
                        }
                    }
                }
                if deletes_left > 0 && rng.chance(15) {
                    let items = pick_deletable(&model.tree, &spec, &shared, &mut rng);
                    if !items.is_empty() {
                        deletes_left -= 1;
                        report.deletes += 1;
                        if let Some(tr) = trace() {
                            for r in &items {
                                if let Some(ix) = model.tree.resolve(*r) {
                                    let p =
                                        String::from_utf8_lossy(&model.tree.path(ix)).into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] delete submitted {p}", report.ticks);
                                    }
                                }
                            }
                        }
                        applier.command(
                            &mut model,
                            Command::Delete {
                                items,
                                mode: Mode::Permanent,
                            },
                            clock.now(),
                        );
                        plans.extend(applier.take_plans().into_iter().map(|p| (p, 0)));
                        if let Err(e) = applier.check(&model) {
                            return fail(format!("invariant after delete: {e}"), &report);
                        }
                    }
                }
                if rescans_left > 0
                    && rng.chance(20)
                    && let Some(node) = random_dir(&model.tree, &mut rng)
                {
                    rescans_left -= 1;
                    report.rescans += 1;
                    let r = model.tree.node_ref(node);
                    if let Some(tr) = trace() {
                        let p = String::from_utf8_lossy(&model.tree.path(node)).into_owned();
                        if tr.starts_with(&p) {
                            eprintln!("[{}] rescan requested {p}", report.ticks);
                        }
                    }
                    applier.command(&mut model, Command::Rescan { node: r }, clock.now());
                    if let Err(e) = applier.check(&model) {
                        return fail(format!("invariant after rescan: {e}"), &report);
                    }
                }
            }
        }
        let (out, ab) = shared.outstanding();
        if out == ab
            && model.status.running() == 0
            && !applier.has_work()
            && cur.is_none()
            && sink.q.lock().is_empty()
            && model.status.commands_applied > 0
            && plans.is_empty()
            && del_q.is_empty()
            && model.status.deleting.is_none()
        {
            // A cancelled job leaves directories incomplete (Cancelled, or Superseded when a
            // cancelled rescan had cut them). Rescan the topmost one, as a user would, until
            // none remain; then the tree must match the filesystem again.
            match topmost_incomplete(&model.tree) {
                Some(ix) if recoveries < 100 => {
                    recoveries += 1;
                    report.recoveries += 1;
                    let r = model.tree.node_ref(ix);
                    applier.command(&mut model, Command::Rescan { node: r }, clock.now());
                }
                Some(_) => {
                    return fail(
                        "incomplete directories remain after 100 rescans".into(),
                        &report,
                    );
                }
                None => break,
            }
        }
    }

    let st = &model.status.stats;
    report.max_budget_used = sink.max_used.load(SeqCst);
    report.stalled_msgs = st.stalled;
    report.late = st.late;
    report.discarded_cut = st.discarded_cut;
    if st.corrupt + st.lost + st.duplicate > 0 {
        return fail(
            format!(
                "corrupt {} lost {} duplicate {}: {:?}",
                st.corrupt,
                st.lost,
                st.duplicate,
                &model.status.anomalies[..model.status.anomalies.len().min(5)]
            ),
            &report,
        );
    }
    if report.max_budget_used > cfg.budget as u64 {
        return fail(
            format!("budget exceeded: {}", report.max_budget_used),
            &report,
        );
    }
    applier
        .check(&model)
        .map_err(|e| format!("seed {seed}: final invariant: {e}"))?;
    let (dirs, abandoned) = compare(&spec, &model.tree, cfg.limits.block_entries).map_err(|e| {
        let n = &model.status.notices;
        format!(
            "seed {seed}: {e}; last notices: {:?}",
            &n[n.len().saturating_sub(6)..]
        )
    })?;
    report.dirs = dirs;
    report.abandoned = abandoned;
    Ok(report)
}

fn trace() -> Option<String> {
    std::env::var("DUT_SIM_TRACE").ok()
}

fn spec_node(shared: &Shared<SimBackend>, tree: &Tree, ix: Ix) -> u32 {
    use dut_scan::Backend as _;
    shared
        .backend
        .loc_from_path(&dut_engine::bytes_to_path(&tree.path(ix)))
}

/// One to three visible entries whose removal cannot shift a fault position.
fn pick_deletable(
    tree: &Tree,
    spec: &Spec,
    shared: &Shared<SimBackend>,
    rng: &mut Rng,
) -> Vec<dut_tree::NodeRef> {
    let Some(root) = tree.roots().next() else {
        return vec![];
    };
    let mut all = vec![];
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        stack.extend(tree.children(ix));
        if ix != root
            && !tree.flags(ix).has(NodeFlags::DELETING)
            && spec.deletable(spec_node(shared, tree, ix))
        {
            all.push(ix);
        }
    }
    let mut out = vec![];
    for _ in 0..=rng.below(3) {
        if all.is_empty() {
            break;
        }
        let i = rng.below(all.len() as u64) as usize;
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
    use dut_scan::Backend as _;
    let (plan, i) = plans.front_mut()?;
    if *i >= plan.items.len() {
        let id = plan.id;
        plans.pop_front();
        return Some(DeleterMsg::Done { plan: id });
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

/// The visible directory closest to the root that a cancelled job left incomplete.
fn topmost_incomplete(tree: &Tree) -> Option<Ix> {
    let root = tree.roots().next()?;
    let mut queue = VecDeque::from([root]);
    while let Some(ix) = queue.pop_front() {
        let d = tree.dir(ix)?;
        if d.cut || matches!(d.state, DirState::Superseded | DirState::Cancelled) {
            return Some(ix);
        }
        queue.extend(tree.children(ix).filter(|&c| tree.dir(c).is_some()));
    }
    None
}

fn random_dir(tree: &Tree, rng: &mut Rng) -> Option<Ix> {
    let root = tree.roots().next()?;
    let mut dirs = vec![];
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        dirs.push(d);
        stack.extend(tree.children(d).filter(|&c| tree.dir(c).is_some()));
    }
    dirs.get(rng.below(dirs.len() as u64) as usize).copied()
}

// ---- reference -------------------------------------------------------------------------------

fn allowed(fault: &Fault, len: usize, state: DirState) -> bool {
    use DirState::*;
    match fault {
        Fault::None => state == Complete,
        // An error after the last entry is never reached.
        Fault::EioAfter(k) if *k >= len => state == Complete,
        Fault::Eacces => state == Failed,
        Fault::EioAfter(0) => state == Failed,
        Fault::EioAfter(_) => state == Partial,
        Fault::PanicAtBlock(_) | Fault::PanicOnOpen => state == Partial,
        Fault::Stall { dur_ns: None, .. } => state == Abandoned,
        Fault::Stall {
            dur_ns: Some(_), ..
        } => matches!(state, Complete | Abandoned),
    }
}

/// Entries of a directory the scan can have seen, given its fault and final state. `len` is the
/// number of live children (faulted directories never lose children, see `Spec::deletable`).
fn prefix(n: &SpecNode, len: usize, state: DirState, block: usize) -> usize {
    match (state, &n.fault) {
        (DirState::Complete, _) => len,
        (DirState::Partial, Fault::EioAfter(k)) => (*k).min(len),
        (DirState::Partial, Fault::PanicAtBlock(b)) => (b * block).min(len),
        (DirState::Abandoned, Fault::Stall { block: b, .. }) => (b * block).min(len),
        _ => 0,
    }
}

/// Compares every directory in the final tree with the spec. Returns (dirs checked, abandoned).
fn compare(spec: &Spec, tree: &Tree, block: usize) -> Result<(usize, u64), String> {
    let root = tree.roots().next().ok_or("no root")?;
    // Final state of every directory, by path.
    let mut by_path: HashMap<Vec<u8>, Ix> = HashMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        by_path.insert(tree.path(ix), ix);
        stack.extend(tree.children(ix));
    }
    let state_of = |p: &[u8]| by_path.get(p).and_then(|&ix| tree.dir(ix)).map(|d| d.state);

    // Which spec entries the scan saw, and the lowest path of every hard-linked inode.
    let root_dev = spec.nodes[ROOT as usize].meta.dev;
    let mut seen: Vec<(u32, Vec<u8>)> = vec![];
    let mut charged: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
    let mut abandoned = 0;
    let mut stack = vec![(ROOT, ROOT_NAME.as_bytes().to_vec())];
    while let Some((d, path)) = stack.pop() {
        let n = &spec.nodes[d as usize];
        seen.push((d, path.clone()));
        if n.meta.kind != Kind::Dir {
            if !n.vanished && n.meta.nlink > 1 {
                let e = charged
                    .entry((n.meta.dev, n.meta.ino))
                    .or_insert_with(|| path.clone());
                if path < *e {
                    *e = path.clone();
                }
            }
            continue;
        }
        if d != ROOT && n.meta.dev != root_dev {
            continue;
        }
        let state =
            state_of(&path).ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&path)))?;
        let fault = if d == ROOT { &Fault::None } else { &n.fault };
        if !allowed(fault, n.children.len(), state) {
            return Err(format!(
                "{}: state {state:?} not allowed for {fault:?}",
                String::from_utf8_lossy(&path)
            ));
        }
        abandoned += (state == DirState::Abandoned) as u64;
        let live: Vec<u32> = spec.live_children(d).collect();
        for &c in &live[..prefix(n, live.len(), state, block)] {
            let mut p = path.clone();
            p.push(SEP);
            p.extend_from_slice(&spec.nodes[c as usize].name);
            stack.push((c, p));
        }
    }

    // Expected totals bottom-up.
    let mut expect: HashMap<Vec<u8>, Totals> = HashMap::new();
    seen.sort_by_key(|s| std::cmp::Reverse(s.1.len()));
    for (d, path) in &seen {
        let n = &spec.nodes[*d as usize];
        let mut t = if n.meta.kind == Kind::Dir || !n.vanished {
            let counted = n.meta.kind == Kind::Dir
                || n.meta.nlink <= 1
                || charged.get(&(n.meta.dev, n.meta.ino)) == Some(path);
            Totals {
                apparent: if counted { n.meta.apparent } else { 0 },
                alloc: if counted { n.meta.alloc } else { 0 },
                items: 1,
                errors: 0,
            }
        } else {
            Totals {
                apparent: 0,
                alloc: 0,
                items: 1,
                errors: 1,
            }
        };
        if n.meta.kind == Kind::Dir {
            if let Some(s) = state_of(path)
                && matches!(
                    s,
                    DirState::Failed | DirState::Partial | DirState::Abandoned
                )
            {
                t.errors += 1;
            }
            let mut prefix_len = path.len() + 1;
            if prefix_len == 0 {
                prefix_len = 1;
            }
            for (p, ct) in expect.iter() {
                if p.len() > prefix_len
                    && p.starts_with(path)
                    && p[path.len()] == SEP
                    && !p[prefix_len..].contains(&SEP)
                {
                    t.add(ct);
                }
            }
        }
        expect.insert(path.clone(), t);
    }

    let mut checked = 0;
    // Deepest first, so the reported mismatch is where the difference starts.
    let mut paths: Vec<&Vec<u8>> = expect.keys().collect();
    paths.sort_by_key(|p| (std::cmp::Reverse(p.len()), (*p).clone()));
    for path in paths {
        let t = &expect[path];
        let ix = *by_path
            .get(path)
            .ok_or_else(|| format!("missing entry {}", String::from_utf8_lossy(path)))?;
        let got = tree.total(ix);
        if got != *t {
            let in_tree: Vec<String> = tree
                .children(ix)
                .map(|c| String::from_utf8_lossy(tree.name(c)).into_owned())
                .collect();
            let mut expected: Vec<String> = expect
                .keys()
                .filter(|p| {
                    p.starts_with(path)
                        && p.len() > path.len()
                        && p[path.len()] == SEP
                        && !p[path.len() + 1..].contains(&SEP)
                })
                .map(|p| String::from_utf8_lossy(&p[path.len() + 1..]).into_owned())
                .collect();
            expected.sort();
            let state = tree.dir(ix).map(|d| d.state);
            if tree.dir(ix).is_none() {
                let key = tree.dev_ino(ix);
                let links: Vec<String> = by_path
                    .iter()
                    .filter(|&(_, &o)| tree.dir(o).is_none() && tree.dev_ino(o) == key)
                    .map(|(p, &o)| {
                        format!(
                            "{} charged={}",
                            String::from_utf8_lossy(p),
                            tree.flags(o).has(NodeFlags::CHARGED)
                        )
                    })
                    .collect();
                return Err(format!(
                    "{}: got {got:?} expected {t:?}; links of inode {key:?}: {links:?}",
                    String::from_utf8_lossy(path)
                ));
            }
            return Err(format!(
                "{}: got {got:?} expected {t:?}; state {state:?}; children {in_tree:?} expected {expected:?}",
                String::from_utf8_lossy(path)
            ));
        }
        checked += (tree.dir(ix).is_some()) as usize;
    }
    if by_path.len() != expect.len() {
        let extra: Vec<String> = by_path
            .keys()
            .filter(|p| !expect.contains_key(*p))
            .take(5)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        return Err(format!(
            "tree has {} entries, expected {}; extra e.g. {extra:?}",
            by_path.len(),
            expect.len()
        ));
    }
    Ok((checked, abandoned))
}
