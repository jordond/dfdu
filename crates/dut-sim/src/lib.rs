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
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use dut_engine::{Applier, Command, Model};
use dut_scan::{
    Backend, Batch, Block, Budget, Clock, DirStream, JobOptions, Limits, ScanMsg, Shared, Sink,
    StepResult, Worker,
};
use dut_tree::{DirState, ErrKind, Ix, Kind, Meta, Totals, Tree};
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
        Spec { nodes }
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
        let limit = match n.fault {
            Fault::EioAfter(k) => k.min(n.children.len()),
            _ => n.children.len(),
        };
        if self.pos >= limit && limit < n.children.len() {
            return Err(io::Error::from_raw_os_error(5));
        }
        let end = (self.pos + max_entries).min(limit);
        for &c in &n.children[self.pos..end] {
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
        if self.pos >= limit && limit < n.children.len() {
            self.eio_pending = true;
            return Ok(true);
        }
        Ok(self.pos < n.children.len())
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
        for part in rest.split('/').filter(|p| !p.is_empty()) {
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
        match self.spec.nodes[*loc as usize].fault {
            Fault::Eacces => return Err(io::ErrorKind::PermissionDenied.into()),
            Fault::PanicOnOpen => panic!("injected panic"),
            _ => {}
        }
        Ok(SimStream {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: *loc,
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
    pub abandoned: u64,
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
    let mut replaced = 0usize;
    let mut applier = Applier::new(shared.clone());
    let mut model = Model::default();
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut rescans_left = cfg.rescans;
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
                    if applier.has_work() {
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
            }
            80..90 => {
                let stuck = shared.watchdog_round(&*sink, clock.now());
                if stuck > replaced && replaced < cfg.workers {
                    replaced += 1;
                    workers.push(Worker::new(&shared, false));
                }
                clock.advance(1_000_000);
            }
            90..95 => applier.tick(&mut model, clock.now(), Some(cfg.auto_abandon_ns)),
            _ => {
                clock.advance(rng.below(3) * 1_000_000);
                if rescans_left > 0
                    && rng.chance(20)
                    && let Some(node) = random_dir(&model.tree, &mut rng)
                {
                    rescans_left -= 1;
                    report.rescans += 1;
                    let r = model.tree.node_ref(node);
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
        {
            break;
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
    let (dirs, abandoned) = compare(&spec, &model.tree, cfg.limits.block_entries)
        .map_err(|e| format!("seed {seed}: {e}"))?;
    report.dirs = dirs;
    report.abandoned = abandoned;
    Ok(report)
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

/// Entries of a directory the scan can have seen, given its fault and final state.
fn prefix(n: &SpecNode, state: DirState, block: usize) -> usize {
    let len = n.children.len();
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
        for &c in &n.children[..prefix(n, state, block)] {
            let mut p = path.clone();
            p.push(b'/');
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
                    && p[path.len()] == b'/'
                    && !p[prefix_len..].contains(&b'/')
                {
                    t.add(ct);
                }
            }
        }
        expect.insert(path.clone(), t);
    }

    let mut checked = 0;
    for (path, t) in &expect {
        let ix = *by_path
            .get(path)
            .ok_or_else(|| format!("missing entry {}", String::from_utf8_lossy(path)))?;
        let got = tree.total(ix);
        if got != *t {
            return Err(format!(
                "{}: got {got:?} expected {t:?}",
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
