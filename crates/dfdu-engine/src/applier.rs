//! The applier core: gate, chunk and close handling, pending accounting, jobs, cuts, rescans,
//! stalls, quiescence and hard links (`docs/data-pipeline.md` §5-§7). No threads: the runtime
//! and the simulation harness both drive it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use dfdu_clean::{DeleteResult, DeleterMsg, Identity, Mode, Plan, PlanItem};
use dfdu_scan::{
    Backend, Chunk, ChunkStatus, DirId, JobId, JobOptions, ListingStatus, ScanMsg, Shared,
    SkipReason,
};
use dfdu_tree::{
    DirState, ErrKind, Ix, Kind, Meta, NodeFlags, NodeRef, Pages, Subvol, Totals, Tree,
};

use crate::extents::{ExtentTable, Inode};
use crate::links::{LinkTable, Scope, VISIBLE};
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use crate::recent::{Event, Recent};

/// What readers see under the read lock: the tree and the engine status, always consistent with
/// each other.
#[derive(Default)]
pub struct Model {
    pub tree: Tree,
    pub status: Status,
    /// btrfs subvolume roots (nodes flagged `SUBVOL`). Entries of freed nodes stop resolving
    /// and are pruned after each job.
    pub subvols: HashMap<NodeRef, Subvol>,
    /// The last messages and commands applied, for crash reports.
    pub recent: Recent,
}

impl Model {
    /// Changes whenever anything a reader can see may have changed. The applier wakes readers
    /// only when it changes; readers can compare it to skip redraws.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let s = &self.status;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.tree.version().hash(&mut h);
        s.stats.messages.hash(&mut h);
        s.commands_applied.hash(&mut h);
        s.notices.len().hash(&mut h);
        s.stalled.len().hash(&mut h);
        s.incomplete.hash(&mut h);
        s.settling.hash(&mut h);
        s.deleting
            .as_ref()
            .map(|d| (d.plan, d.done, d.queued_plans))
            .hash(&mut h);
        s.last_delete.as_ref().map(|d| d.plan).hash(&mut h);
        s.measuring
            .as_ref()
            .map(|p| (p.done, p.requested))
            .hash(&mut h);
        for j in &s.jobs {
            (j.id.0, j.phase as u8).hash(&mut h);
        }
        h.finish()
    }
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

/// How the applier thread spends its time, filled in by the runtime (D1, ROADMAP M2): how
/// long it holds the write lock per slice, how long it waits to get it, and in total. Not part
/// of [`Model::fingerprint`], so it never wakes readers by itself.
#[derive(Clone, Debug)]
pub struct Timing {
    pub slices: u64,
    /// Time holding the write lock, all slices together.
    pub locked_ns: u64,
    /// Write-lock hold time per slice, in [`Timing::BUCKETS`] buckets of [`Timing::bucket`].
    pub hold: [u64; Timing::BUCKETS],
    /// Time waiting for the write lock per slice, same buckets.
    pub wait: [u64; Timing::BUCKETS],
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            slices: 0,
            locked_ns: 0,
            hold: [0; Timing::BUCKETS],
            wait: [0; Timing::BUCKETS],
        }
    }
}

impl Timing {
    /// Four buckets per power of two, up to about 35 minutes.
    pub const BUCKETS: usize = 128;

    /// The bucket of a duration: whole microseconds below 4 µs, then four buckets per power of
    /// two (4-5 µs, 5-6, 6-7, 7-8, 8-10, ...), so a bucket's bounds are within 25% of each
    /// other.
    pub fn bucket(d: std::time::Duration) -> usize {
        let us = d.as_micros().min(u64::MAX as u128) as u64;
        if us < 4 {
            return us as usize;
        }
        let e = (u64::BITS - 1 - us.leading_zeros()) as usize;
        let m = ((us >> (e - 2)) & 3) as usize;
        (4 * (e - 1) + m).min(Self::BUCKETS - 1)
    }

    /// The upper bound of bucket `k` (exclusive).
    pub fn upper(k: usize) -> std::time::Duration {
        let us = if k < 4 {
            k as u64 + 1
        } else {
            (5 + (k % 4) as u64) << (k / 4 - 1)
        };
        std::time::Duration::from_micros(us)
    }

    /// Upper bound of the `p`-th percentile (0 to 1) of a histogram.
    pub fn percentile(hist: &[u64; Self::BUCKETS], p: f64) -> std::time::Duration {
        let total: u64 = hist.iter().sum();
        let want = (total as f64 * p).ceil() as u64;
        let mut seen = 0;
        for (k, &n) in hist.iter().enumerate() {
            seen += n;
            if seen >= want.max(1) {
                return Self::upper(k);
            }
        }
        std::time::Duration::ZERO
    }

    /// Upper bound of the longest time in a histogram.
    pub fn max(hist: &[u64; Self::BUCKETS]) -> std::time::Duration {
        hist.iter()
            .rposition(|&n| n > 0)
            .map_or(std::time::Duration::ZERO, Self::upper)
    }

    /// Entries of a histogram at or above `d`, rounded up to a bucket bound (2 ms counts
    /// from 2.048 ms).
    pub fn at_least(hist: &[u64; Self::BUCKETS], d: std::time::Duration) -> u64 {
        let k = Self::bucket(d);
        let k = if k > 0 && Self::upper(k - 1) < d {
            k + 1
        } else {
            k
        };
        hist[k.min(Self::BUCKETS)..].iter().sum()
    }
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
    pub timing: Timing,
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
    /// The running delete plan, if any.
    pub deleting: Option<DeleteProgress>,
    /// Set when the applier thread failed; the model no longer changes. Readers should report it
    /// and exit.
    pub fatal: Option<String>,
    /// Outcome of the most recent finished plan.
    pub last_delete: Option<DeleteSummary>,
    /// Shared-extent measurement in progress (`docs/btrfs.md` §3.4).
    pub measuring: Option<MeasureProgress>,
    /// Files whose extents could not be measured; their shared data may be counted more than
    /// once.
    pub unmeasured: u64,
    /// Some measured file has compressed extents whose size on disk is not known (not root).
    pub compressed_upper_bound: bool,
    /// Measurements came from the extent tree (as root): sizes on disk are exact.
    pub exact_sizes: bool,
    /// btrfs data profile of the scan root's filesystem and its raw bytes per byte of data.
    pub profile: Option<(&'static str, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct MeasureProgress {
    pub done: u64,
    pub requested: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DeleteProgress {
    pub plan: u64,
    pub total: usize,
    pub done: usize,
    pub queued_plans: usize,
}

#[derive(Clone, Debug, Default)]
pub struct DeleteSummary {
    pub plan: u64,
    pub deleted: usize,
    pub bytes: u64,
    pub stale: usize,
    pub refused: usize,
    pub failed: usize,
    pub aborted: usize,
    /// Something was deleted on btrfs, which frees the space when the transaction commits.
    pub btrfs: bool,
}

impl Status {
    pub fn running(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| matches!(j.phase, JobPhase::Running | JobPhase::Finishing))
            .count()
    }

    pub fn busy(&self) -> bool {
        self.running() > 0 || self.settling || self.deleting.is_some() || self.measuring.is_some()
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
    /// Delete these visible nodes (§7.4). Plans run one at a time.
    Delete {
        items: Vec<NodeRef>,
        mode: Mode,
    },
    /// Drop queued delete plans. The runtime also stops the running plan after its current item.
    AbortDelete,
    /// Panics inside the applier, to test crash handling.
    #[cfg(feature = "fault-injection")]
    InjectPanic,
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
    map: Pages<Slot>,
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
    /// A rescan that has not swapped in yet: it still counts for the overlap rules (§7.2),
    /// including while it settles hard links after its scan finished.
    fn swap_pending(&self) -> bool {
        matches!(self.phase, JobPhase::Running | JobPhase::Finishing)
    }

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
            self.map.grow_to(i + 1, Slot::Unknown);
        }
        self.map[i] = s;
    }
    fn set(&mut self, d: DirId, r: NodeRef) {
        self.put(d, Slot::Live(r));
    }
}

enum Work {
    Free,
    /// Drop freed links from these inodes' lists ([`LinkTable::prune`]).
    Prune {
        keys: Vec<(u64, u64)>,
    },
    Reelect {
        key: (u64, u64),
        scope: Scope,
    },
    FinalPass {
        keys: Vec<(u64, u64)>,
        scope: Scope,
    },
    /// A final pass over the visible scope, with its keys taken when it starts (after any
    /// merges queued before it).
    FinalPassVisible,
    Merge {
        keys: Vec<(u64, u64)>,
        from: Scope,
    },
    CancelWalk {
        job: u32,
        stack: Vec<NodeRef>,
    },
    LostWalk {
        job: u32,
        stack: Vec<NodeRef>,
    },
    FinishJob {
        job: u32,
    },
}

pub struct Applier<B: Backend> {
    pub scan: Arc<Shared<B>>,
    jobs: BTreeMap<u32, Job>,
    next_job: u32,
    links: LinkTable,
    work: VecDeque<Work>,
    retry_abandon: Vec<(JobId, DirId)>,
    queued_plans: VecDeque<Plan>,
    active_plan: Option<ActivePlan>,
    outgoing_plans: Vec<Plan>,
    next_plan: u64,
    /// Rescans needed after a delete that are blocked by the running plan; retried as it runs.
    deferred_rescans: Vec<NodeRef>,
    /// Refuse every delete request.
    pub read_only: bool,
    /// Measure extents after each job (`docs/btrfs.md` §3.4).
    pub measure: bool,
    extents: ExtentTable,
    /// Devices whose filesystem can share extents, from the scanner's probes.
    extent_devs: HashSet<u64>,
    measure_stack: Vec<NodeRef>,
    outgoing_measure: Vec<MeasureReq>,
    measure_inflight: usize,
    /// Results of earlier rounds (abandoned after a timeout) are ignored.
    measure_round: u64,
    measure_last: u64,
}

/// Files in measurement requests in flight at most.
const MEASURE_INFLIGHT: usize = 32768;
/// Files per measurement request (one directory per request).
const MEASURE_BATCH: usize = 256;
/// A measurement round without any result for this long is abandoned.
const MEASURE_TIMEOUT_NS: u64 = 60_000_000_000;

struct ActivePlan {
    id: u64,
    total: usize,
    remaining: HashSet<NodeRef>,
    summary: DeleteSummary,
}

/// Units of incremental work per step (nodes freed, nodes walked).
const STEP_UNITS: usize = 4096;

/// Hard links examined per step of re-election or merging: whole inodes are taken until this
/// many of their links have been looked at. Re-electing compares the paths of all of an inode's
/// links. Counting inodes instead (256 per step) held the write lock for up to 65-130 ms on
/// a tree of 2,000 inodes with 500 links each, against a slice of 1 ms (§5.6). 2048 links took
/// 2.0-2.4 ms per step on the 1M-entry `dfdu-bench` tree (9,565 links), one step per slice at the
/// end of each scan; 512 take about 0.5 ms there, but 2-2.5 ms on a 10M-entry tree, where each
/// link's walk up the tree misses the cache. 128 keep a step well under a slice at 10M.
const LINKS_PER_STEP: usize = 128;

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
        mount: 0,
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
        mount: 0,
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
            queued_plans: VecDeque::new(),
            active_plan: None,
            outgoing_plans: Vec::new(),
            next_plan: 1,
            deferred_rescans: Vec::new(),
            read_only: false,
            measure: true,
            extents: ExtentTable::default(),
            extent_devs: HashSet::new(),
            measure_stack: Vec::new(),
            outgoing_measure: Vec::new(),
            measure_inflight: 0,
            measure_round: 0,
            measure_last: 0,
        }
    }

    pub fn has_work(&self) -> bool {
        !self.work.is_empty() || self.measure_ready()
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
        if self.measure_stack.is_empty() && self.measure_inflight == 0 {
            m.status.measuring = None;
        }
    }

    // ---- commands --------------------------------------------------------------------------

    pub fn command(&mut self, m: &mut Model, cmd: Command, now: u64) {
        m.recent.push(Event::of_command(&cmd));
        match cmd {
            Command::Scan { path, opts } => {
                let id = self.next_job;
                self.next_job += 1;
                let name = dfdu_tree::name_bytes(path.as_os_str()).into_owned();
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
            Command::Delete { items, mode } => self.submit_plan(m, items, mode),
            #[cfg(feature = "fault-injection")]
            Command::InjectPanic => panic!("injected applier panic"),
            Command::AbortDelete => {
                for p in std::mem::take(&mut self.queued_plans) {
                    for it in &p.items {
                        if let Some(ix) = m.tree.resolve(it.node) {
                            m.tree.set_deleting(ix, false);
                        }
                    }
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
            map: Pages::default(),
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
            Some(_) if m.tree.flags(ix).has(NodeFlags::DUP_MOUNT) => {
                m.status
                    .notice("rescan: that directory is counted at another mount of it".into());
                return;
            }
            Some(_) if m.tree.flags(ix).has(NodeFlags::KERNEL) => {
                m.status
                    .notice("rescan: that is a kernel filesystem, which is never scanned".into());
                return;
            }
            Some(_) if m.tree.flags(ix).has(NodeFlags::NETWORK) => {
                m.status.notice(
                    "rescan: that is a network filesystem; start dfdu with --network to scan it"
                        .into(),
                );
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
        if self.plan_overlaps(&m.tree, ix) {
            m.status.notice(
                "rescan: a delete in this directory is in progress; rescan after it completes"
                    .into(),
            );
            return;
        }
        let mut cancel = Vec::new();
        for j in self
            .jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.swap_pending())
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
        let path = m.tree.path_buf(ix);
        let shadow = m.tree.new_root(
            m.tree.name(ix).to_vec().as_slice(),
            &meta_of(&m.tree, ix),
            false,
        );
        // Whether the directory is a mount root depends on its parent, which the rescan keeps.
        if m.tree.flags(ix).has(NodeFlags::MOUNT_ROOT) {
            m.tree.set_flag(shadow, NodeFlags::MOUNT_ROOT, true);
        }
        let mut opts = self
            .jobs
            .get(&m.tree.dir(ix).expect("dir").job)
            .map(|j| j.opts.clone())
            .unwrap_or_default();
        // Duplicate mounts are judged against the whole visible tree, not just this part.
        let mut top = ix;
        while let Some(p) = m.tree.parent(top) {
            top = p;
        }
        opts.scope = Some(m.tree.path_buf(top));
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

    fn scanner_abandon(&mut self, job: JobId, dir: DirId) {
        if !self.scan.abandon(job, dir) {
            self.retry_abandon.push((job, dir));
        }
    }

    fn under_cut(tree: &Tree, ix: Ix) -> bool {
        let mut cur = Some(ix);
        while let Some(c) = cur {
            if tree.dir(c).is_some_and(|d| d.cut) {
                return true;
            }
            cur = tree.parent(c);
        }
        false
    }

    /// Periodic work: retry abandons that could not take an outbox lock, and abandon
    /// directories stalled longer than `auto_abandon_ns` (headless mode).
    pub fn tick(&mut self, m: &mut Model, now: u64, auto_abandon_ns: Option<u64>) {
        self.measure_watchdog(m, now);
        let scan = &self.scan;
        self.retry_abandon.retain(|(j, d)| !scan.abandon(*j, *d));
        // Stalled directories that were cut or removed since they stalled are not wanted.
        let mut unwanted = Vec::new();
        for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
            for &d in j.stalled.keys() {
                let gone = j
                    .get(DirId(d))
                    .and_then(|r| m.tree.resolve(r))
                    .is_none_or(|ix| Self::under_cut(&m.tree, ix));
                if gone {
                    unwanted.push((j.id, DirId(d)));
                }
            }
        }
        for (job, dir) in unwanted {
            if let Some(j) = self.jobs.get_mut(&job.0) {
                j.stalled.remove(&dir.0);
            }
            self.scanner_abandon(job, dir);
        }
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
        m.recent.push(Event::of_msg(&msg));
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
                match self.gate(m, job, dir, None) {
                    Some(ix) => {
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
                    // Nothing wants this directory any more (job ended, subtree cut or gone):
                    // give the stuck task up so it stops counting as outstanding work.
                    None => self.scanner_abandon(JobId(job.0), dir),
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
        // Gone, detached, or under a cut.
        if let Some(ix) = r.and_then(|r| m.tree.resolve_uncut(r)) {
            return Some(ix);
        }
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
        None
    }

    fn apply_chunk(&mut self, m: &mut Model, c: Chunk) {
        m.status.stats.chunks += 1;
        let Some(ix) = self.gate(m, c.job, c.dir, Some((c.first_subdir, c.subdirs.len()))) else {
            return;
        };
        let rec = m.tree.dir_mut(ix).expect("dir");
        if rec.listing_closed {
            // From a part that returned after the directory was abandoned. Its subdirectories
            // were never added, but tasks for them exist: stop them, and drop what they send
            // (as under a cut, §5.1).
            m.status.stats.late += 1;
            let ids: Vec<DirId> = (0..c.subdirs.len() as u32)
                .map(|i| DirId(c.first_subdir.0 + i))
                .collect();
            let j = self.jobs.get_mut(&c.job.0).expect("gated job");
            for &d in &ids {
                j.put(d, Slot::Dead);
            }
            self.scan.stop_dirs(c.job, ids);
            return;
        }
        if !rec.mark_seq(c.seq) {
            m.status.stats.duplicate += 1;
            return;
        }
        if let Some(meta) = c.root_meta {
            m.tree.update_meta(ix, &meta);
        }
        if let Some(p) = c.probe.as_deref() {
            if p.shares_extents {
                self.extent_devs.insert(m.tree.dev_ino(ix).0);
            }
            if p.profile.is_some() && m.tree.flags(ix).has(NodeFlags::VISIBLE_ROOT) {
                m.status.profile = p.profile;
            }
            if let Some(sv) = p.subvol.clone() {
                m.tree.set_flag(ix, NodeFlags::SUBVOL, true);
                m.tree.set_flag(ix, NodeFlags::RO_SUBVOL, sv.read_only);
                m.subvols.insert(m.tree.node_ref(ix), sv);
            }
        }
        m.tree
            .reserve_children(ix, c.files.len() + c.errors.len() + c.subdirs.len());
        let j = self.jobs.get_mut(&c.job.0).expect("gated job");
        let scope = j.scope;
        let mut delta = Totals::default();
        for f in &c.files {
            let name = c.name(f.name);
            let fx = if f.meta.is_hardlink() {
                let mut fx = Ix(0);
                self.links
                    .add_scanned((f.meta.dev, f.meta.ino), scope, |charged| {
                        fx = m.tree.add_file(ix, name, &f.meta, charged, None);
                        m.tree.node_ref(fx)
                    });
                fx
            } else {
                m.tree.add_file(ix, name, &f.meta, false, None)
            };
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
                mount: 0,
            };
            let fx = m
                .tree
                .add_file(ix, c.name(e.name), &meta, false, Some(e.err));
            delta.add(&m.tree.own(fx));
        }
        let mut new_pending = 0;
        for (i, s) in c.subdirs.iter().enumerate() {
            let id = DirId(c.first_subdir.0 + i as u32);
            let mut flags = 0;
            if s.mount_root {
                flags |= NodeFlags::MOUNT_ROOT;
            }
            match s.skip {
                Some(SkipReason::DuplicateMount) => flags |= NodeFlags::DUP_MOUNT,
                Some(SkipReason::Network) => flags |= NodeFlags::NETWORK,
                Some(SkipReason::Kernel) => flags |= NodeFlags::KERNEL,
                _ => {}
            }
            let cx = m.tree.add_dir_flagged(ix, c.name(s.name), &s.meta, flags);
            let r = m.tree.node_ref(cx);
            j.set(id, r);
            let crec = m.tree.dir_mut(cx).expect("dir");
            crec.job = c.job.0;
            crec.dir_id = id.0;
            if s.skip.is_some() {
                crec.state = DirState::Skipped;
                crec.close(DirState::Skipped);
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
        rec.close(outcome);
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
            if self.measure_ready() {
                self.measure_step(m);
            }
            return;
        };
        match w {
            Work::Free => {
                let mut freed = Vec::new();
                let done = m.tree.free_step(STEP_UNITS, |f| freed.push(f));
                let mut prune = Vec::new();
                for f in freed {
                    let key = (f.dev, f.ino);
                    if f.hardlink {
                        let (reelect, prune_due) = self.links.remove(key, f.node);
                        if let Some(scope) = reelect {
                            self.work.push_front(Work::Reelect { key, scope });
                        }
                        if prune_due {
                            prune.push(key);
                        }
                        self.sync_extents(m, key, false);
                    } else if f.measured && self.extents.pos(key) == Some(f.node) {
                        self.extents.remove(&mut m.tree, key);
                    }
                }
                if !done {
                    self.work.push_back(Work::Free);
                }
                if !prune.is_empty() {
                    self.work.push_front(Work::Prune { keys: prune });
                }
            }
            Work::Prune { mut keys } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    n += self.links.prune(&m.tree, k).max(1);
                    self.sync_extents(m, k, false);
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::Prune { keys });
                }
            }
            Work::Reelect { key, scope } => {
                let (moved, _) = self.links.reelect(&mut m.tree, key, scope);
                self.sync_extents(m, key, moved);
            }
            Work::FinalPass { mut keys, scope } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    let (moved, links) = self.links.reelect(&mut m.tree, k, scope);
                    n += links.max(1);
                    self.sync_extents(m, k, moved);
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::FinalPass { keys, scope });
                }
            }
            Work::FinalPassVisible => {
                let tree = &m.tree;
                m.subvols.retain(|r, _| tree.resolve(*r).is_some());
                let keys = self.links.shared_keys(VISIBLE);
                self.work.push_front(Work::FinalPass {
                    keys,
                    scope: VISIBLE,
                });
            }
            Work::Merge { mut keys, from } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    let (reelect, links) = self.links.merge_key(&mut m.tree, k, from, VISIBLE);
                    n += links.max(1);
                    self.sync_extents(m, k, true);
                    if reelect {
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
                        rec.close(DirState::Cancelled);
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
        j.map = Pages::default();
        j.held_close.clear();
        let stalled: Vec<DirId> = j.stalled.drain().map(|(d, _)| DirId(d)).collect();
        let job = j.id;
        for d in stalled {
            self.scanner_abandon(job, d);
        }
        if kind == JobKind::Initial {
            self.start_measure(m, root);
        }
        if kind == JobKind::Rescan {
            let shadow = m.tree.resolve(root);
            match (cancelled, shadow, m.tree.resolve_visible(target)) {
                (false, Some(s), Some(t)) => {
                    m.tree.replace(t, s);
                    self.start_measure(m, root);
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
        // Every job's end (done or cancelled) settles the visible charges to the lowest path, so
        // per-directory totals never depend on scan order (P5, §5.5).
        self.work.push_back(Work::FinalPassVisible);
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

    // ---- extent measurement (docs/btrfs.md §3.4) ---------------------------------------------

    fn start_measure(&mut self, m: &mut Model, root: NodeRef) {
        if !self.measure || self.extent_devs.is_empty() {
            return;
        }
        m.status.measuring.get_or_insert_with(Default::default);
        self.measure_stack.push(root);
    }

    fn measure_ready(&self) -> bool {
        !self.measure_stack.is_empty() && self.measure_inflight < MEASURE_INFLIGHT
    }

    /// Walks part of a measured subtree, requesting each file that can share extents and is
    /// not measured yet (for hard links, the charged link).
    fn measure_step(&mut self, m: &mut Model) {
        let tree = &m.tree;
        let mut units = 0;
        while units < STEP_UNITS && self.measure_inflight < MEASURE_INFLIGHT {
            let Some(r) = self.measure_stack.pop() else {
                break;
            };
            units += 1;
            let Some(ix) = tree.resolve_visible(r) else {
                continue;
            };
            let mut files: Vec<MeasureFile> = Vec::new();
            for c in tree.children(ix) {
                units += 1;
                if tree.dir(c).is_some() {
                    self.measure_stack.push(tree.node_ref(c));
                    continue;
                }
                let f = tree.flags(c);
                let inode = tree.dev_ino(c);
                if tree.kind(c) != Kind::File
                    || f.has(NodeFlags::MEASURED)
                    || tree.raw_size(c).1 == 0
                    || !self.extent_devs.contains(&inode.0)
                    || (f.has(NodeFlags::HARDLINK) && !f.has(NodeFlags::CHARGED))
                    || self
                        .extents
                        .pos(inode)
                        .is_some_and(|p| tree.resolve_visible(p).is_some())
                {
                    continue;
                }
                files.push(MeasureFile {
                    node: tree.node_ref(c),
                    name: tree.name(c).to_vec(),
                    inode,
                });
            }
            if !files.is_empty() {
                self.measure_inflight += files.len();
                m.status
                    .measuring
                    .get_or_insert_with(Default::default)
                    .requested += files.len() as u64;
                let dir = tree.path_buf(ix);
                for part in files.chunks(MEASURE_BATCH) {
                    self.outgoing_measure.push(MeasureReq {
                        round: self.measure_round,
                        dir: dir.clone(),
                        files: part.to_vec(),
                    });
                }
            }
        }
    }

    /// Requests one file (a hard link whose charge moved to it).
    fn request_one(&mut self, m: &mut Model, node: NodeRef, inode: Inode) {
        let tree = &m.tree;
        let Some(ix) = tree.resolve_visible(node) else {
            return;
        };
        let Some(parent) = tree.parent(ix) else {
            return;
        };
        self.outgoing_measure.push(MeasureReq {
            round: self.measure_round,
            dir: tree.path_buf(parent),
            files: vec![MeasureFile {
                node,
                name: tree.name(ix).to_vec(),
                inode,
            }],
        });
        self.measure_inflight += 1;
        m.status
            .measuring
            .get_or_insert_with(Default::default)
            .requested += 1;
    }

    /// Requests for the measurers, taken by the runtime after each slice.
    pub fn take_measure_requests(&mut self) -> Vec<MeasureReq> {
        std::mem::take(&mut self.outgoing_measure)
    }

    pub fn apply_measure(&mut self, m: &mut Model, msg: MeasureMsg, now: u64) {
        if msg.round != self.measure_round {
            return;
        }
        self.measure_inflight = self.measure_inflight.saturating_sub(1);
        self.measure_last = now;
        if let Some(p) = m.status.measuring.as_mut() {
            p.done += 1;
        }
        let tree = &m.tree;
        let still = tree.resolve_visible(msg.node).is_some_and(|ix| {
            let f = tree.flags(ix);
            tree.dev_ino(ix) == msg.inode
                && (!f.has(NodeFlags::HARDLINK) || f.has(NodeFlags::CHARGED))
        });
        match msg.result {
            MeasureResult::Measured(me) if still => {
                m.status.compressed_upper_bound |= me.compressed;
                m.status.exact_sizes |= me.exact;
                self.extents.measure(&mut m.tree, msg.inode, msg.node, me);
            }
            MeasureResult::Measured(_) => {
                // A hard link whose charge moved to another link meanwhile: measure that one,
                // or the inode would stay unmeasured.
                let tree = &m.tree;
                let measured = self
                    .extents
                    .pos(msg.inode)
                    .is_some_and(|p| tree.resolve_visible(p).is_some());
                if let Some(c) = self.links.charged(msg.inode, VISIBLE)
                    && c != msg.node
                    && !measured
                {
                    self.request_one(m, c, msg.inode);
                }
            }
            MeasureResult::Changed => {}
            MeasureResult::Failed(_) => m.status.unmeasured += 1,
        }
        // The part of `refresh_status` a result can change; the whole of it, once per result,
        // took a tenth of the applier's time while measuring.
        if self.measure_stack.is_empty() && self.measure_inflight == 0 {
            m.status.measuring = None;
        }
    }

    /// Keeps an inode's measurement on its charged link as hard-link charges move, and drops
    /// it when no link remains. An inode not measured yet whose charge `moved` to a link that
    /// can be measured is requested: the walk skipped that link while it was uncharged, and a
    /// result for the old link is ignored.
    fn sync_extents(&mut self, m: &mut Model, key: Inode, moved: bool) {
        let Some(pos) = self.extents.pos(key) else {
            if moved
                && self.measure
                && self.extent_devs.contains(&key.0)
                && let Some(c) = self.links.charged(key, VISIBLE)
                && m.tree
                    .resolve_visible(c)
                    .is_some_and(|cx| m.tree.raw_size(cx).1 > 0)
            {
                self.request_one(m, c, key);
            }
            return;
        };
        match self.links.charged(key, VISIBLE) {
            Some(c) if c != pos => self.extents.move_to(&mut m.tree, key, c),
            Some(_) => {}
            None if !self.links.has(key) => self.extents.remove(&mut m.tree, key),
            // A re-election is pending.
            None => {}
        }
    }

    /// Abandons a measurement round that has had no result for a while (a filesystem not
    /// answering), so the engine does not stay busy forever.
    fn measure_watchdog(&mut self, m: &mut Model, now: u64) {
        if self.measure_inflight == 0 {
            self.measure_last = now;
            return;
        }
        if now.saturating_sub(self.measure_last) < MEASURE_TIMEOUT_NS {
            return;
        }
        m.status.unmeasured += self.measure_inflight as u64;
        m.status.notice(
            "extent measurement stopped: a filesystem did not answer; shared data may be \
             counted more than once"
                .into(),
        );
        self.measure_round += 1;
        self.measure_inflight = 0;
        self.measure_stack.clear();
        self.measure_last = now;
    }

    // ---- deletion (§7.2, §7.4) --------------------------------------------------------------

    /// Plans for the deleter thread, taken by the runtime after each slice.
    pub fn take_plans(&mut self) -> Vec<Plan> {
        std::mem::take(&mut self.outgoing_plans)
    }

    fn plan_nodes(&self) -> impl Iterator<Item = NodeRef> + '_ {
        let active = self
            .active_plan
            .iter()
            .flat_map(|p| p.remaining.iter().copied());
        let queued = self
            .queued_plans
            .iter()
            .flat_map(|p| p.items.iter().map(|i| i.node));
        active.chain(queued)
    }

    /// Whether `ix`, an ancestor or a descendant is in the running or a queued plan.
    fn plan_overlaps(&self, tree: &Tree, ix: Ix) -> bool {
        self.plan_nodes()
            .filter_map(|r| tree.resolve(r))
            .any(|p| p == ix || Self::is_ancestor(tree, p, ix) || Self::is_ancestor(tree, ix, p))
    }

    /// Why the tree says `ix` cannot be deleted (`docs/btrfs.md` §3.6); the deleter checks the
    /// filesystem again before touching it.
    fn delete_refusal(tree: &Tree, ix: Ix) -> Option<String> {
        let flags = tree.flags(ix);
        let t = tree.total(ix);
        if flags.has(NodeFlags::MOUNT_ROOT) {
            return Some("is a mount point; unmount it first".into());
        }
        if t.mounts > 0 {
            return Some(format!(
                "contains {} mount point{}; unmount {} first",
                t.mounts,
                if t.mounts == 1 { "" } else { "s" },
                if t.mounts == 1 { "it" } else { "them" }
            ));
        }
        let mut a = ix;
        while let Some(p) = tree.parent(a) {
            if tree.flags(p).has(NodeFlags::RO_SUBVOL) {
                return Some(format!(
                    "is in the read-only snapshot {}",
                    String::from_utf8_lossy(tree.name(p))
                ));
            }
            a = p;
        }
        if t.ro_subvols > 0 && !dfdu_sys::is_privileged() {
            return Some(if flags.has(NodeFlags::RO_SUBVOL) {
                "is a read-only snapshot: deleting it needs root".into()
            } else {
                "contains a read-only snapshot: deleting it needs root".into()
            });
        }
        None
    }

    fn submit_plan(&mut self, m: &mut Model, items: Vec<NodeRef>, mode: Mode) {
        if self.read_only {
            m.status.notice("delete: read-only mode".into());
            return;
        }
        let mut out = Vec::new();
        let mut seen: HashSet<Ix> = HashSet::new();
        for r in items {
            let Some(ix) = m.tree.resolve_visible(r) else {
                continue;
            };
            if m.tree.flags(ix).has(NodeFlags::DELETING) || !seen.insert(ix) {
                continue;
            }
            // Overlap with rescans (§7.2): a running rescan of the item, an ancestor or a
            // descendant blocks it. Cancelling a descendant's rescan instead would leave that
            // directory cut, with partial totals, if this delete then did not happen.
            let blocked = self
                .jobs
                .values()
                .filter(|j| j.kind == JobKind::Rescan && j.swap_pending())
                .filter_map(|j| m.tree.resolve_visible(j.target))
                .any(|t| {
                    t == ix
                        || Self::is_ancestor(&m.tree, t, ix)
                        || Self::is_ancestor(&m.tree, ix, t)
                });
            if blocked {
                m.status.notice(format!(
                    "delete: {} or something in it is being rescanned; delete after it completes",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            if let Some(why) = Self::delete_refusal(&m.tree, ix) {
                m.status.notice(format!(
                    "delete: {} {why}",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            if m.tree
                .dir(ix)
                .is_some_and(|d| d.state == DirState::Pending && !d.cut)
            {
                let (job, dir) = {
                    let d = m.tree.dir(ix).expect("dir");
                    (d.job, d.dir_id)
                };
                self.cut(m, ix, DirState::Deleting);
                self.scan.stop_dirs(JobId(job), [DirId(dir)]);
            }
            m.tree.set_deleting(ix, true);
            let (dev, ino) = m.tree.dev_ino(ix);
            out.push(PlanItem {
                node: m.tree.node_ref(ix),
                path: m.tree.path_buf(ix),
                identity: Identity {
                    dev,
                    ino,
                    kind: m.tree.kind(ix),
                    apparent: m.tree.raw_size(ix).0,
                    mtime: m.tree.mtime(ix),
                },
                bytes: m.tree.total(ix).alloc,
            });
        }
        if out.is_empty() {
            return;
        }
        let root = out
            .first()
            .and_then(|i| m.tree.resolve(i.node))
            .map(|mut ix| {
                while let Some(p) = m.tree.parent(ix) {
                    ix = p;
                }
                m.tree.path_buf(ix)
            })
            .unwrap_or_default();
        let id = self.next_plan;
        self.next_plan += 1;
        self.queued_plans.push_back(Plan {
            id,
            mode,
            scan_root: root,
            items: out,
        });
        self.start_next_plan(m);
    }

    fn start_next_plan(&mut self, m: &mut Model) {
        if self.active_plan.is_some() {
            if let Some(d) = m.status.deleting.as_mut() {
                d.queued_plans = self.queued_plans.len();
            }
            return;
        }
        while let Some(mut plan) = self.queued_plans.pop_front() {
            // Re-check items queued behind another plan: they may have gone meanwhile.
            plan.items
                .retain(|i| m.tree.resolve_visible(i.node).is_some());
            if plan.items.is_empty() {
                continue;
            }
            let remaining: HashSet<NodeRef> = plan.items.iter().map(|i| i.node).collect();
            m.status.deleting = Some(DeleteProgress {
                plan: plan.id,
                total: remaining.len(),
                done: 0,
                queued_plans: self.queued_plans.len(),
            });
            self.active_plan = Some(ActivePlan {
                id: plan.id,
                total: remaining.len(),
                remaining,
                summary: DeleteSummary {
                    plan: plan.id,
                    ..Default::default()
                },
            });
            self.outgoing_plans.push(plan);
            return;
        }
        m.status.deleting = None;
    }

    fn retry_deferred_rescans(&mut self, m: &mut Model) {
        for r in std::mem::take(&mut self.deferred_rescans) {
            match m.tree.resolve_visible(r) {
                None => {}
                Some(ix) if self.plan_overlaps(&m.tree, ix) => self.deferred_rescans.push(r),
                Some(_) => self.rescan(m, r),
            }
        }
    }

    /// Applies a result from the deleter (§7.4 steps 3-5).
    pub fn apply_delete(&mut self, m: &mut Model, msg: DeleterMsg) {
        match msg {
            DeleterMsg::Item {
                plan,
                node,
                bytes,
                result,
            } => {
                let Some(active) = self.active_plan.as_mut().filter(|a| a.id == plan) else {
                    return;
                };
                active.remaining.remove(&node);
                let s = &mut active.summary;
                let ix = m.tree.resolve(node);
                let mut rescan = false;
                match &result {
                    DeleteResult::Deleted => {
                        s.deleted += 1;
                        s.bytes += bytes;
                        if let Some(ix) = ix {
                            m.tree.detach(ix);
                            self.work.push_back(Work::Free);
                        }
                    }
                    other => {
                        match other {
                            DeleteResult::Stale(_) => s.stale += 1,
                            DeleteResult::Refused(_) => s.refused += 1,
                            DeleteResult::Aborted => s.aborted += 1,
                            _ => s.failed += 1,
                        }
                        if let Some(ix) = ix {
                            m.tree.set_deleting(ix, false);
                            let was_cut = m
                                .tree
                                .dir(ix)
                                .is_some_and(|d| d.cut && d.state == DirState::Deleting);
                            let partial =
                                matches!(other, DeleteResult::Failed { partial: true, .. });
                            // A directory on another filesystem shows none of its contents, so
                            // a part-way delete inside it leaves nothing stale.
                            let skipped =
                                m.tree.dir(ix).is_some_and(|d| d.state == DirState::Skipped);
                            rescan = (was_cut || partial) && !skipped;
                            if rescan {
                                // Its totals are no longer known. Marking it cut keeps it shown
                                // as incomplete if the rescan below is cancelled or never runs.
                                if let Some(d) = m.tree.dir_mut(ix) {
                                    d.cut = true;
                                }
                                m.tree.touch(ix);
                            }
                        }
                        let name = ix
                            .map(|ix| String::from_utf8_lossy(m.tree.name(ix)).into_owned())
                            .unwrap_or_default();
                        m.status.notice(format!("delete {name}: {other}"));
                    }
                }
                let done = active.total - active.remaining.len();
                if let Some(d) = m.status.deleting.as_mut() {
                    d.done = done;
                }
                if rescan {
                    // Its totals are no longer known: the scan was stopped or part of it is gone.
                    self.deferred_rescans.push(node);
                }
                self.retry_deferred_rescans(m);
            }
            DeleterMsg::Done { plan, btrfs } => {
                if btrfs {
                    m.status.notice(
                        "btrfs frees the space when the transaction commits; space of deleted \
                         snapshots is freed in the background, so free space can lag"
                            .into(),
                    );
                }
                if self.active_plan.as_ref().is_some_and(|a| a.id == plan) {
                    let a = self.active_plan.take().expect("active");
                    for r in a.remaining {
                        if let Some(ix) = m.tree.resolve(r) {
                            m.tree.set_deleting(ix, false);
                        }
                    }
                    m.status.last_delete = Some(DeleteSummary { btrfs, ..a.summary });
                }
                self.retry_deferred_rescans(m);
                self.start_next_plan(m);
            }
        }
        self.refresh_status(m);
    }

    // ---- invariants ------------------------------------------------------------------------

    /// Checks I1 and I3 (tree), I2 and I4 (accounting) for running jobs. Linear in tree size.
    /// The extent table's view of one inode, for debugging.
    pub fn describe_extents(&self, m: &Model, inode: Inode) -> String {
        self.extents.describe(&m.tree, inode)
    }

    /// Checks what must hold once nothing is pending: every measured inode is counted at a
    /// visible node.
    pub fn check_settled(&self, m: &Model) -> Result<(), String> {
        self.extents.check_settled(&m.tree)
    }

    pub fn check(&self, m: &Model) -> Result<(), String> {
        m.tree
            .check(&self.shadow_roots(&m.tree))
            .map_err(|v| format!("{v:?}"))?;
        self.extents.check(&m.tree)?;
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
                        m.tree.path_buf(ix).display(),
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

#[cfg(test)]
mod tests {
    use super::Timing;
    use std::time::Duration;

    #[test]
    fn timing_buckets_hold_their_durations() {
        let mut last = 0;
        for us in 0..100_000u64 {
            let d = Duration::from_micros(us);
            let k = Timing::bucket(d);
            assert!(k == last || k == last + 1, "{us} µs");
            last = k;
            assert!(d < Timing::upper(k), "{us} µs");
            assert!(k == 0 || d >= Timing::upper(k - 1), "{us} µs");
        }
        let mut h = [0; Timing::BUCKETS];
        h[Timing::bucket(Duration::from_micros(2047))] = 1;
        h[Timing::bucket(Duration::from_micros(2048))] = 10;
        assert_eq!(Timing::at_least(&h, Duration::from_millis(2)), 10);
        assert_eq!(
            Timing::bucket(Duration::from_secs(1 << 40)),
            Timing::BUCKETS - 1
        );
    }
}
