//! The applier core: gate, chunk and close handling, pending accounting, jobs, cuts, rescans,
//! stalls, quiescence and hard links (`docs/data-pipeline.md` §5-§7). No threads: the runtime
//! and the simulation harness both drive it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use dut_scan::{
    Backend, Chunk, ChunkStatus, DirId, JobId, JobOptions, ListingStatus, ScanMsg, Shared,
};
use dut_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeRef, Totals, Tree};

use crate::links::{LinkTable, Scope, VISIBLE};

/// What readers see under the read lock: the tree and the engine status, always consistent with
/// each other.
#[derive(Default)]
pub struct Model {
    pub tree: Tree,
    pub status: Status,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub messages: u64,
    pub chunks: u64,
    pub closes: u64,
    pub entries: u64,
    pub discarded_job: u64,
    pub discarded_cut: u64,
    pub corrupt: u64,
    pub late: u64,
    pub duplicate: u64,
    pub lost: u64,
    pub stalled: u64,
    pub quiescent: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JobKind {
    Initial,
    Rescan,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JobPhase {
    Running,
    Finishing,
    Done,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct JobInfo {
    pub id: JobId,
    pub kind: JobKind,
    /// The visible node the job is about: the scan root, or the rescanned directory.
    pub target: NodeRef,
    pub phase: JobPhase,
}

#[derive(Clone, Debug)]
pub struct StalledInfo {
    pub node: NodeRef,
    pub since_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub jobs: Vec<JobInfo>,
    pub stalled: Vec<StalledInfo>,
    pub stats: Stats,
    /// Some directory ended `Lost`, or an invariant check failed: totals may be incomplete.
    pub incomplete: bool,
    /// Incremental work (freeing, hard-link re-election) is pending.
    pub settling: bool,
    /// User-visible notices, newest last, at most 100.
    pub notices: Vec<String>,
    /// Commands processed so far; lets a caller know its command has taken effect.
    pub commands_applied: u64,
    /// Details of protocol anomalies (corrupt, duplicate, lost), at most 50, for debugging.
    pub anomalies: Vec<String>,
}

impl Status {
    pub fn running(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| matches!(j.phase, JobPhase::Running | JobPhase::Finishing))
            .count()
    }

    pub fn busy(&self) -> bool {
        self.running() > 0 || self.settling
    }

    fn anomaly(&mut self, s: String) {
        if self.anomalies.len() < 50 {
            self.anomalies.push(s);
        }
    }

    fn notice(&mut self, s: String) {
        if self.notices.len() >= 100 {
            self.notices.remove(0);
        }
        self.notices.push(s);
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Start a scan of `path` as a new visible root.
    Scan {
        path: PathBuf,
        opts: JobOptions,
    },
    /// Rescan a visible directory (§7.3).
    Rescan {
        node: NodeRef,
    },
    Cancel {
        job: JobId,
    },
    CancelAll,
    /// Give up on a stalled directory (§5.4).
    Abandon {
        node: NodeRef,
    },
    AbandonAllStalled,
}

struct Job {
    id: JobId,
    kind: JobKind,
    target: NodeRef,
    root: NodeRef,
    scope: Scope,
    opts: JobOptions,
    map: Vec<Slot>,
    held_close: HashMap<u32, (u32, ListingStatus)>,
    stalled: HashMap<u32, u64>,
    submitted_seq: u64,
    phase: JobPhase,
}

/// What the applier knows about a `DirId` of a job.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Slot {
    /// Not named by any applied chunk yet.
    Unknown,
    Live(NodeRef),
    /// Named by a chunk that was discarded (its directory was cut, deleted or abandoned), so
    /// it has no node and never will; its messages are discarded.
    Dead,
}

impl Job {
    fn slot(&self, d: DirId) -> Slot {
        self.map.get(d.0 as usize).copied().unwrap_or(Slot::Unknown)
    }
    fn get(&self, d: DirId) -> Option<NodeRef> {
        match self.slot(d) {
            Slot::Live(r) => Some(r),
            _ => None,
        }
    }
    fn put(&mut self, d: DirId, s: Slot) {
        let i = d.0 as usize;
        if self.map.len() <= i {
            self.map.resize(i + 1, Slot::Unknown);
        }
        self.map[i] = s;
    }
    fn set(&mut self, d: DirId, r: NodeRef) {
        self.put(d, Slot::Live(r));
    }
}

enum Work {
    Free,
    Reelect { key: (u64, u64), scope: Scope },
    FinalPass { keys: Vec<(u64, u64)>, scope: Scope },
    Merge { keys: Vec<(u64, u64)>, from: Scope },
    CancelWalk { job: u32, stack: Vec<NodeRef> },
    LostWalk { job: u32, stack: Vec<NodeRef> },
    FinishJob { job: u32 },
}

pub struct Applier<B: Backend> {
    pub scan: Arc<Shared<B>>,
    jobs: BTreeMap<u32, Job>,
    next_job: u32,
    links: LinkTable,
    work: VecDeque<Work>,
    retry_abandon: Vec<(JobId, DirId)>,
}

/// Units of incremental work per step (nodes freed, nodes walked, inodes merged).
const STEP_UNITS: usize = 4096;

pub fn meta_of(tree: &Tree, ix: Ix) -> Meta {
    let (apparent, alloc) = tree.raw_size(ix);
    let (dev, ino) = tree.dev_ino(ix);
    Meta {
        kind: tree.kind(ix),
        apparent,
        alloc,
        mtime: tree.mtime(ix),
        dev,
        ino,
        nlink: 1,
    }
}

pub fn bytes_to_path(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

fn empty_dir_meta() -> Meta {
    Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 0,
        ino: 0,
        nlink: 1,
    }
}

impl<B: Backend> Applier<B> {
    pub fn new(scan: Arc<Shared<B>>) -> Self {
        Applier {
            scan,
            jobs: BTreeMap::new(),
            next_job: 1,
            links: LinkTable::default(),
            work: VecDeque::new(),
            retry_abandon: Vec::new(),
        }
    }

    pub fn has_work(&self) -> bool {
        !self.work.is_empty()
    }

    /// Roots of running rescan shadows, for the structural invariant check.
    pub fn shadow_roots(&self, tree: &Tree) -> Vec<Ix> {
        self.jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.phase != JobPhase::Done)
            .filter_map(|j| tree.resolve(j.root))
            .filter(|&ix| !tree.is_visible(ix))
            .collect()
    }

    pub fn refresh_status(&self, m: &mut Model) {
        m.status.jobs = self
            .jobs
            .values()
            .map(|j| JobInfo {
                id: j.id,
                kind: j.kind,
                target: j.target,
                phase: j.phase,
            })
            .collect();
        let mut stalled = Vec::new();
        for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
            for (&d, &since) in &j.stalled {
                if let Some(r) = j.get(DirId(d)) {
                    stalled.push(StalledInfo {
                        node: r,
                        since_ns: since,
                    });
                }
            }
        }
        m.status.stalled = stalled;
        m.status.settling = !self.work.is_empty();
    }

    // ---- commands --------------------------------------------------------------------------

    pub fn command(&mut self, m: &mut Model, cmd: Command, now: u64) {
        match cmd {
            Command::Scan { path, opts } => {
                let id = self.next_job;
                self.next_job += 1;
                let name = path.as_os_str().as_encoded_bytes().to_vec();
                let root = m.tree.new_root(&name, &empty_dir_meta(), true);
                self.start(m, id, JobKind::Initial, root, root, VISIBLE, path, opts);
            }
            Command::Rescan { node } => self.rescan(m, node),
            Command::Cancel { job } => self.cancel(m, job.0),
            Command::CancelAll => {
                let ids: Vec<u32> = self.jobs.keys().copied().collect();
                for id in ids {
                    self.cancel(m, id);
                }
            }
            Command::Abandon { node } => self.abandon(m, node),
            Command::AbandonAllStalled => {
                for s in m.status.stalled.clone() {
                    self.abandon(m, s.node);
                }
            }
        }
        let _ = now;
        m.status.commands_applied += 1;
        self.refresh_status(m);
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        m: &mut Model,
        id: u32,
        kind: JobKind,
        target: Ix,
        root: Ix,
        scope: Scope,
        path: PathBuf,
        opts: JobOptions,
    ) {
        let rec = m.tree.dir_mut(root).expect("job root is a directory");
        rec.job = id;
        rec.dir_id = 0;
        let mut job = Job {
            id: JobId(id),
            kind,
            target: m.tree.node_ref(target),
            root: m.tree.node_ref(root),
            scope,
            opts: opts.clone(),
            map: Vec::new(),
            held_close: HashMap::new(),
            stalled: HashMap::new(),
            submitted_seq: 0,
            phase: JobPhase::Running,
        };
        job.set(DirId::ROOT, m.tree.node_ref(root));
        job.submitted_seq = self.scan.start_job(JobId(id), &path, opts);
        self.jobs.insert(id, job);
    }

    fn is_ancestor(tree: &Tree, a: Ix, mut b: Ix) -> bool {
        while let Some(p) = tree.parent(b) {
            if p == a {
                return true;
            }
            b = p;
        }
        false
    }

    fn rescan(&mut self, m: &mut Model, node: NodeRef) {
        let Some(ix) = m.tree.resolve_visible(node) else {
            m.status.notice("rescan: that directory is gone".into());
            return;
        };
        match m.tree.dir(ix) {
            None => {
                m.status.notice("rescan: not a directory".into());
                return;
            }
            Some(d) if d.state == DirState::Skipped => {
                m.status
                    .notice("rescan: that directory is on another filesystem".into());
                return;
            }
            Some(_) => {}
        }
        // Overlap rules (§7.2).
        let mut cancel = Vec::new();
        for j in self
            .jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.phase == JobPhase::Running)
        {
            let Some(t) = m.tree.resolve_visible(j.target) else {
                continue;
            };
            if t == ix || Self::is_ancestor(&m.tree, t, ix) {
                m.status
                    .notice("rescan: already rescanning this directory or a parent".into());
                return;
            }
            if Self::is_ancestor(&m.tree, ix, t) {
                cancel.push(j.id.0);
            }
        }
        for id in cancel {
            self.cancel(m, id);
        }
        // If the directory is still being scanned by its job, cut it there.
        let rec = m.tree.dir(ix).expect("dir");
        if rec.state == DirState::Pending && !rec.cut {
            let (job, dir) = (rec.job, rec.dir_id);
            self.cut(m, ix, DirState::Superseded);
            self.scan.stop_dirs(JobId(job), [DirId(dir)]);
        }
        let id = self.next_job;
        self.next_job += 1;
        let path = bytes_to_path(&m.tree.path(ix));
        let shadow = m.tree.new_root(
            m.tree.name(ix).to_vec().as_slice(),
            &meta_of(&m.tree, ix),
            false,
        );
        let opts = self
            .jobs
            .get(&m.tree.dir(ix).expect("dir").job)
            .map(|j| j.opts.clone())
            .unwrap_or_default();
        self.start(m, id, JobKind::Rescan, ix, shadow, id, path, opts);
    }

    /// Cuts a still-scanning subtree: no more data from its job, and it reports to its parent
    /// at once with `state` (§7.1).
    fn cut(&mut self, m: &mut Model, ix: Ix, state: DirState) {
        let rec = m.tree.dir_mut(ix).expect("dir");
        rec.cut = true;
        rec.state = state;
        rec.outcome = state;
        let job = rec.job;
        if !rec.reported {
            rec.reported = true;
            self.report_up(m, job, ix);
        }
        m.tree.touch(ix);
    }

    fn cancel(&mut self, m: &mut Model, id: u32) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        if j.phase != JobPhase::Running {
            return;
        }
        j.phase = JobPhase::Cancelled;
        self.scan.end_job(j.id);
        let root = j.root;
        self.work.push_back(Work::CancelWalk {
            job: id,
            stack: vec![root],
        });
        self.work.push_back(Work::FinishJob { job: id });
        let _ = m;
    }

    fn abandon(&mut self, m: &mut Model, node: NodeRef) {
        let Some(ix) = m.tree.resolve(node) else {
            return;
        };
        let Some(rec) = m.tree.dir(ix) else { return };
        let (job, dir) = (rec.job, rec.dir_id);
        if rec.listing_closed || rec.cut {
            return;
        }
        if let Some(j) = self.jobs.get_mut(&job) {
            j.stalled.remove(&dir);
        }
        m.tree.set_error(ix, ErrKind::Stalled);
        self.close_listing(m, job, ix, DirState::Abandoned);
        if !self.scan.abandon(JobId(job), DirId(dir)) {
            self.retry_abandon.push((JobId(job), DirId(dir)));
        }
    }

    /// Periodic work: retry abandons that could not take an outbox lock, and abandon
    /// directories stalled longer than `auto_abandon_ns` (headless mode).
    pub fn tick(&mut self, m: &mut Model, now: u64, auto_abandon_ns: Option<u64>) {
        let scan = &self.scan;
        self.retry_abandon.retain(|(j, d)| !scan.abandon(*j, *d));
        if let Some(limit) = auto_abandon_ns {
            let mut due = Vec::new();
            for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
                for (&d, &since) in &j.stalled {
                    if now.saturating_sub(since) > limit
                        && let Some(r) = j.get(DirId(d))
                    {
                        due.push(r);
                    }
                }
            }
            for r in due {
                self.abandon(m, r);
            }
        }
        self.refresh_status(m);
    }

    // ---- messages --------------------------------------------------------------------------

    /// Applies one scanner message.
    pub fn apply(&mut self, m: &mut Model, msg: ScanMsg, now: u64) {
        m.status.stats.messages += 1;
        match msg {
            ScanMsg::Chunk(c) => self.apply_chunk(m, c),
            ScanMsg::Close {
                job,
                dir,
                chunks,
                status,
            } => self.apply_close(m, job, dir, chunks, status),
            ScanMsg::Stalled { job, dir } => {
                m.status.stats.stalled += 1;
                if let Some(ix) = self.gate(m, job, dir, None) {
                    let rec = m.tree.dir_mut(ix).expect("dir");
                    if !rec.listing_closed {
                        rec.stalled = true;
                        if let Some(j) = self.jobs.get_mut(&job.0) {
                            j.stalled.insert(dir.0, now);
                        }
                        m.tree.touch(ix);
                        self.refresh_status(m);
                    }
                }
            }
            ScanMsg::Quiescent { submitted_seq } => {
                m.status.stats.quiescent += 1;
                for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
                    if j.submitted_seq > submitted_seq {
                        continue;
                    }
                    let Some(root) = m.tree.resolve(j.root) else {
                        continue;
                    };
                    if m.tree.dir(root).is_some_and(|r| r.pending > 0) {
                        self.work.push_back(Work::LostWalk {
                            job: j.id.0,
                            stack: vec![j.root],
                        });
                    }
                }
                self.refresh_status(m);
            }
        }
    }

    /// §5.1. Returns the directory node if the message should be applied.
    fn gate(
        &mut self,
        m: &mut Model,
        job: JobId,
        dir: DirId,
        subdirs: Option<(DirId, usize)>,
    ) -> Option<Ix> {
        let st = &mut m.status.stats;
        let Some(j) = self.jobs.get(&job.0) else {
            st.discarded_job += 1;
            return None;
        };
        if j.phase != JobPhase::Running {
            st.discarded_job += 1;
            return None;
        }
        let r = match j.slot(dir) {
            Slot::Live(r) => Some(r),
            Slot::Dead => None,
            Slot::Unknown => {
                st.corrupt += 1;
                m.status.incomplete = true;
                m.status
                    .anomaly(format!("unknown dir: job {} dir {}", job.0, dir.0));
                return None;
            }
        };
        let cut = match r.and_then(|r| m.tree.resolve(r)) {
            None => true,
            Some(ix) => {
                let mut cur = Some(ix);
                let mut cut = false;
                while let Some(c) = cur {
                    if m.tree.dir(c).is_some_and(|d| d.cut) {
                        cut = true;
                        break;
                    }
                    cur = m.tree.parent(c);
                }
                if !cut {
                    return Some(ix);
                }
                cut
            }
        };
        if cut {
            st.discarded_cut += 1;
            let mut stop = vec![dir];
            if let Some((first, n)) = subdirs {
                stop.extend((0..n as u32).map(|i| DirId(first.0 + i)));
            }
            let j = self.jobs.get_mut(&job.0).expect("checked above");
            for &d in &stop[1..] {
                j.put(d, Slot::Dead);
            }
            self.scan.stop_dirs(job, stop);
        }
        None
    }

    fn apply_chunk(&mut self, m: &mut Model, c: Chunk) {
        m.status.stats.chunks += 1;
        let Some(ix) = self.gate(m, c.job, c.dir, Some((c.first_subdir, c.subdirs.len()))) else {
            return;
        };
        let rec = m.tree.dir_mut(ix).expect("dir");
        if rec.listing_closed {
            m.status.stats.late += 1;
            return;
        }
        if !rec.mark_seq(c.seq) {
            m.status.stats.duplicate += 1;
            return;
        }
        if let Some(meta) = c.root_meta {
            m.tree.update_meta(ix, &meta);
        }
        let j = self.jobs.get_mut(&c.job.0).expect("gated job");
        let scope = j.scope;
        let mut delta = Totals::default();
        for f in &c.files {
            let name = c.name(f.name);
            let charged = if f.meta.is_hardlink() {
                self.links.should_charge((f.meta.dev, f.meta.ino), scope)
            } else {
                false
            };
            let fx = m.tree.add_file(ix, name, &f.meta, charged, None);
            if f.meta.is_hardlink() {
                let r = m.tree.node_ref(fx);
                self.links.add((f.meta.dev, f.meta.ino), scope, r, charged);
            }
            delta.add(&m.tree.own(fx));
        }
        for e in &c.errors {
            let meta = Meta {
                kind: Kind::Other,
                apparent: 0,
                alloc: 0,
                mtime: 0,
                dev: 0,
                ino: 0,
                nlink: 1,
            };
            let fx = m
                .tree
                .add_file(ix, c.name(e.name), &meta, false, Some(e.err));
            delta.add(&m.tree.own(fx));
        }
        let mut new_pending = 0;
        for (i, s) in c.subdirs.iter().enumerate() {
            let id = DirId(c.first_subdir.0 + i as u32);
            let cx = m.tree.add_dir(ix, c.name(s.name), &s.meta);
            let r = m.tree.node_ref(cx);
            j.set(id, r);
            let crec = m.tree.dir_mut(cx).expect("dir");
            crec.job = c.job.0;
            crec.dir_id = id.0;
            if s.skip.is_some() {
                crec.state = DirState::Skipped;
                crec.outcome = DirState::Skipped;
                crec.listing_closed = true;
                crec.pending = 0;
                crec.reported = true;
            } else {
                new_pending += 1;
            }
            delta.add(&m.tree.own(cx));
        }
        m.tree.propagate_add(ix, &delta);
        m.status.stats.entries += (c.files.len() + c.errors.len() + c.subdirs.len()) as u64;
        let rec = m.tree.dir_mut(ix).expect("dir");
        rec.pending += new_pending;
        rec.chunks_seen += 1;
        if c.status != ChunkStatus::Ok {
            m.tree.touch(ix);
        }
        let seen = m.tree.dir(ix).expect("dir").chunks_seen;
        let j = self.jobs.get_mut(&c.job.0).expect("gated job");
        if j.held_close.get(&c.dir.0).is_some_and(|(n, _)| *n == seen) {
            let (_, status) = j.held_close.remove(&c.dir.0).expect("held");
            self.close_with_status(m, c.job.0, ix, status);
        }
    }

    fn apply_close(
        &mut self,
        m: &mut Model,
        job: JobId,
        dir: DirId,
        chunks: u32,
        status: ListingStatus,
    ) {
        m.status.stats.closes += 1;
        let Some(ix) = self.gate(m, job, dir, None) else {
            return;
        };
        let rec = m.tree.dir(ix).expect("dir");
        if rec.listing_closed {
            // After an abandon, the stuck worker's own Close is expected and late.
            if rec.outcome == DirState::Abandoned {
                m.status.stats.late += 1;
            } else {
                m.status.stats.duplicate += 1;
            }
            return;
        }
        let seen = rec.chunks_seen;
        let j = self.jobs.get_mut(&job.0).expect("gated job");
        if j.held_close.contains_key(&dir.0) {
            m.status.stats.duplicate += 1;
            return;
        }
        if seen == chunks {
            self.close_with_status(m, job.0, ix, status);
        } else if seen > chunks {
            m.status.stats.corrupt += 1;
            m.status.incomplete = true;
            m.status.anomaly(format!(
                "close with {chunks} chunks after {seen}: job {} dir {}",
                job.0, dir.0
            ));
            self.close_with_status(m, job.0, ix, status);
        } else {
            j.held_close.insert(dir.0, (chunks, status));
        }
    }

    fn close_with_status(&mut self, m: &mut Model, job: u32, ix: Ix, status: ListingStatus) {
        let outcome = match status {
            ListingStatus::Complete => DirState::Complete,
            ListingStatus::Partial {
                read_error,
                panicked,
                ..
            } => {
                if let Some(e) = read_error {
                    m.tree.set_error(ix, e);
                } else if panicked > 0 {
                    m.tree.set_error(ix, ErrKind::Other);
                }
                DirState::Partial
            }
            ListingStatus::Failed(e) => {
                m.tree.set_error(ix, e);
                DirState::Failed
            }
            ListingStatus::Cancelled => DirState::Cancelled,
        };
        self.close_listing(m, job, ix, outcome);
    }

    /// §5.3: closes the directory's own listing and decrements its pending count.
    fn close_listing(&mut self, m: &mut Model, job: u32, ix: Ix, outcome: DirState) {
        let rec = m.tree.dir_mut(ix).expect("dir");
        debug_assert!(!rec.listing_closed);
        rec.listing_closed = true;
        rec.outcome = outcome;
        let dir = rec.dir_id;
        if rec.stalled {
            rec.stalled = false;
            if let Some(j) = self.jobs.get_mut(&job) {
                j.stalled.remove(&dir);
            }
            self.refresh_status(m);
        }
        // Marker, sibling and degrade rules run here once the rules engine exists (M5).
        self.dec_pending(m, job, ix);
    }

    /// Decrements `ix`'s pending count and cascades completion upward (§5.3, I2, I4).
    fn dec_pending(&mut self, m: &mut Model, job: u32, ix: Ix) {
        let rec = m.tree.dir_mut(ix).expect("dir");
        debug_assert!(rec.pending > 0, "pending underflow");
        rec.pending = rec.pending.saturating_sub(1);
        if rec.pending > 0 {
            return;
        }
        rec.state = rec.outcome;
        m.tree.touch(ix);
        let rec = m.tree.dir_mut(ix).expect("dir");
        if rec.reported {
            return;
        }
        rec.reported = true;
        self.report_up(m, job, ix);
    }

    fn report_up(&mut self, m: &mut Model, job: u32, ix: Ix) {
        let is_root = self
            .jobs
            .get(&job)
            .is_some_and(|j| m.tree.resolve(j.root) == Some(ix));
        if is_root {
            self.root_done(job);
            return;
        }
        let Some(p) = m.tree.parent(ix) else { return };
        let Some(prec) = m.tree.dir(p) else { return };
        if prec.job != job || prec.cut || prec.reported {
            return;
        }
        self.dec_pending(m, job, p);
    }

    fn root_done(&mut self, job: u32) {
        let Some(j) = self.jobs.get_mut(&job) else {
            return;
        };
        if j.phase != JobPhase::Running {
            return;
        }
        j.phase = JobPhase::Finishing;
        let keys = self.links.shared_keys(j.scope);
        self.work.push_back(Work::FinalPass {
            keys,
            scope: j.scope,
        });
        self.work.push_back(Work::FinishJob { job });
    }

    // ---- incremental work ------------------------------------------------------------------

    /// Runs one bounded unit of incremental work.
    pub fn work_step(&mut self, m: &mut Model) {
        let Some(w) = self.work.pop_front() else {
            return;
        };
        match w {
            Work::Free => {
                let mut freed = Vec::new();
                let done = m.tree.free_step(STEP_UNITS, |f| freed.push(f));
                for f in freed {
                    if let Some(scope) = self.links.remove((f.dev, f.ino), f.node) {
                        self.work.push_front(Work::Reelect {
                            key: (f.dev, f.ino),
                            scope,
                        });
                    }
                }
                if !done {
                    self.work.push_back(Work::Free);
                }
            }
            Work::Reelect { key, scope } => self.links.reelect(&mut m.tree, key, scope),
            Work::FinalPass { mut keys, scope } => {
                for _ in 0..STEP_UNITS.min(keys.len()) {
                    let k = keys.pop().expect("non-empty");
                    self.links.reelect(&mut m.tree, k, scope);
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::FinalPass { keys, scope });
                }
            }
            Work::Merge { mut keys, from } => {
                for _ in 0..STEP_UNITS.min(keys.len()) {
                    let k = keys.pop().expect("non-empty");
                    if self.links.merge_key(&mut m.tree, k, from, VISIBLE) {
                        self.work.push_back(Work::Reelect {
                            key: k,
                            scope: VISIBLE,
                        });
                    }
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::Merge { keys, from });
                }
            }
            Work::CancelWalk { job, mut stack } => {
                let mut n = 0;
                while let Some(r) = stack.pop() {
                    n += 1;
                    let Some(ix) = m.tree.resolve(r) else {
                        continue;
                    };
                    let Some(rec) = m.tree.dir_mut(ix) else {
                        continue;
                    };
                    if rec.job != job || rec.cut {
                        continue;
                    }
                    if rec.state == DirState::Pending {
                        rec.state = DirState::Cancelled;
                        rec.outcome = DirState::Cancelled;
                        rec.listing_closed = true;
                        rec.pending = 0;
                        rec.reported = true;
                        rec.stalled = false;
                        m.tree.touch(ix);
                        let kids: Vec<NodeRef> = m
                            .tree
                            .children(ix)
                            .filter(|&c| m.tree.dir(c).is_some())
                            .map(|c| m.tree.node_ref(c))
                            .collect();
                        stack.extend(kids);
                    }
                    if n >= STEP_UNITS {
                        break;
                    }
                }
                if !stack.is_empty() {
                    self.work.push_front(Work::CancelWalk { job, stack });
                }
            }
            Work::LostWalk { job, mut stack } => {
                let mut n = 0;
                while let Some(r) = stack.pop() {
                    n += 1;
                    let Some(ix) = m.tree.resolve(r) else {
                        continue;
                    };
                    let Some(rec) = m.tree.dir(ix) else { continue };
                    if rec.job != job || rec.cut || rec.state.is_terminal() {
                        continue;
                    }
                    let kids: Vec<NodeRef> = m
                        .tree
                        .children(ix)
                        .filter(|&c| m.tree.dir(c).is_some())
                        .map(|c| m.tree.node_ref(c))
                        .collect();
                    let (closed, stalled) = (rec.listing_closed, rec.stalled);
                    stack.extend(kids);
                    if !closed && !stalled {
                        m.status.stats.lost += 1;
                        m.status.incomplete = true;
                        m.tree.set_error(ix, ErrKind::Other);
                        self.close_listing(m, job, ix, DirState::Lost);
                    }
                    if n >= STEP_UNITS {
                        break;
                    }
                }
                if !stack.is_empty() {
                    self.work.push_front(Work::LostWalk { job, stack });
                }
            }
            Work::FinishJob { job } => self.finish_job(m, job),
        }
        self.refresh_status(m);
    }

    fn finish_job(&mut self, m: &mut Model, id: u32) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        let cancelled = j.phase == JobPhase::Cancelled;
        self.scan.end_job(j.id);
        let (kind, root, target, scope) = (j.kind, j.root, j.target, j.scope);
        j.phase = if cancelled {
            JobPhase::Cancelled
        } else {
            JobPhase::Done
        };
        j.map = Vec::new();
        j.held_close.clear();
        j.stalled.clear();
        if kind == JobKind::Rescan {
            let shadow = m.tree.resolve(root);
            match (cancelled, shadow, m.tree.resolve_visible(target)) {
                (false, Some(s), Some(t)) => {
                    m.tree.replace(t, s);
                    let keys = self.links.keys_in(scope);
                    self.work.push_back(Work::Free);
                    self.work.push_back(Work::Merge { keys, from: scope });
                }
                (_, Some(s), _) => {
                    m.tree.detach(s);
                    self.work.push_back(Work::Free);
                }
                _ => {}
            }
        }
        // Keep finished jobs out of the status list once nothing refers to them.
        if self.jobs.len() > 64 {
            let done: Vec<u32> = self
                .jobs
                .iter()
                .filter(|(_, j)| matches!(j.phase, JobPhase::Done | JobPhase::Cancelled))
                .map(|(&k, _)| k)
                .take(self.jobs.len() - 64)
                .collect();
            for k in done {
                self.jobs.remove(&k);
            }
        }
    }

    // ---- invariants ------------------------------------------------------------------------

    /// Checks I1 and I3 (tree), I2 and I4 (accounting) for running jobs. Linear in tree size.
    pub fn check(&self, m: &Model) -> Result<(), String> {
        m.tree
            .check(&self.shadow_roots(&m.tree))
            .map_err(|v| format!("{v:?}"))?;
        for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
            let Some(root) = m.tree.resolve(j.root) else {
                continue;
            };
            let mut stack = vec![root];
            while let Some(ix) = stack.pop() {
                let rec = m.tree.dir(ix).expect("dir");
                if rec.job != j.id.0 || rec.cut {
                    continue;
                }
                let mut expect = (!rec.listing_closed) as u32;
                for c in m.tree.children(ix) {
                    if let Some(cr) = m.tree.dir(c) {
                        if cr.job == j.id.0 && !cr.reported {
                            expect += 1;
                        }
                        stack.push(c);
                    }
                }
                if rec.pending != expect {
                    return Err(format!(
                        "I2: {:?} pending {} expected {} (job {})",
                        String::from_utf8_lossy(&m.tree.path(ix)),
                        rec.pending,
                        expect,
                        j.id.0
                    ));
                }
                if (rec.pending == 0) != rec.state.is_terminal() {
                    return Err(format!(
                        "I2: pending {} with state {:?}",
                        rec.pending, rec.state
                    ));
                }
            }
        }
        Ok(())
    }
}
