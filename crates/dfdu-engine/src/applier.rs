//! The applier core: gate, chunk and close handling, pending accounting, jobs, cuts, rescans,
//! stalls, quiescence and hard links. No threads: the runtime and the simulation harness both
//! drive it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{
    Backend, Chunk, ChunkStatus, DirId, Entry, JobId, JobOptions, ListingStatus, Mark, ScanMsg,
    Shared, SkipReason,
};
use dfdu_tree::{
    Class, DirState, ErrKind, Ix, Kind, Meta, NodeFlags, NodeRef, Pages, Subvol, Totals, Tree,
};

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::{ExtentTable, Inode};
use crate::links::{LinkTable, Scope, VISIBLE};
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use crate::recent::{Event, Recent};

/// A `Close` that arrived before some of its directory's chunks: their number, the status, and
/// the classes it carries.
type HeldClose = (u32, (ListingStatus, Vec<(DirId, Class)>));

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
    /// The rules the scan classifies against; a node's [`dfdu_tree::Class`] indexes them.
    pub rules: Option<Arc<dfdu_rules::RuleSet>>,
    /// What each rule's native `size_command` reported, by rule ([`Command::NativeSizes`]).
    pub native_sizes: HashMap<u16, Result<u64, String>>,
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
        s.staged.hash(&mut h);
        s.undo.as_ref().map(|u| (u.plan, u.items)).hash(&mut h);
        s.undoing.hash(&mut h);
        s.estimate.as_ref().map(|e| e.id).hash(&mut h);
        s.native
            .as_ref()
            .map(|n| (n.id, n.lines.len(), n.done.is_some()))
            .hash(&mut h);
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

/// How the applier thread spends its time, filled in by the runtime: how long it holds the
/// write lock per slice, how long it waits to get it, and in total. Not part of
/// [`Model::fingerprint`], so it never wakes readers by itself.
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
    /// Set when a remote scan's link ended: why. The jobs running then stopped where they were
    /// (the scan is `incomplete`), and nothing more is scanned.
    pub disconnected: Option<String>,
    /// Outcome of the most recent finished plan.
    pub last_delete: Option<DeleteSummary>,
    /// Items staged and not deleted yet, and their disk usage as scanned.
    pub staged: (usize, u64),
    /// The latest plan whose items can still be put back ([`Command::Undo`]).
    pub undo: Option<UndoInfo>,
    /// An undo is running for this plan.
    pub undoing: Option<u64>,
    /// The answer to the latest [`Command::Estimate`].
    pub estimate: Option<Estimate>,
    /// The latest native cleanup command ([`Command::RunNative`]).
    pub native: Option<NativeRun>,
    /// Shared-extent measurement in progress.
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

/// What deleting a set of nodes is expected to free (architecture §7): their disk usage,
/// less what stays because other files use it too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// The id the estimate was asked for with.
    pub id: u64,
    pub bytes: u64,
    /// Targets counted (those inside another are counted with it) and the nodes in them.
    pub items: usize,
    pub nodes: u64,
    /// Hard-linked files with links outside the targets: their data stays.
    pub linked_out: u64,
    /// Data shared with files outside the scan (reflinks, snapshots): it stays.
    pub shared_out: u64,
    /// A target is still being scanned: `bytes` is a lower bound.
    pub scanning: bool,
    /// Files may share data the scan does not see (APFS clones, or reflinks not measured), so
    /// deleting may free less.
    pub may_free_less: bool,
}

/// A native cleanup command and what it printed.
#[derive(Clone, Debug)]
pub struct NativeRun {
    pub id: u64,
    pub rule: u16,
    /// The command as a person would type it, and where it runs.
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub dry_run: bool,
    /// Its output, the last [`NATIVE_LINES`] lines.
    pub lines: VecDeque<String>,
    /// `None` while it runs.
    pub done: Option<Result<(), String>>,
}

/// Lines of a native command's output kept.
pub const NATIVE_LINES: usize = 2000;
/// How long a native cleanup command may run.
const NATIVE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug)]
pub struct UndoInfo {
    pub plan: u64,
    pub mode: Mode,
    /// Items the undo would put back.
    pub items: usize,
    pub bytes: u64,
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
    pub mode: Option<Mode>,
    pub deleted: usize,
    pub bytes: u64,
    pub stale: usize,
    pub refused: usize,
    pub failed: usize,
    pub aborted: usize,
    /// Something was deleted on btrfs, which frees the space when the transaction commits.
    pub btrfs: bool,
    /// The items not deleted and why, in the order their results came, at most
    /// [`PROBLEMS_KEPT`].
    pub problems: Vec<(NodeRef, String)>,
}

/// Items not deleted that a [`DeleteSummary`] lists.
pub const PROBLEMS_KEPT: usize = 1000;

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
    /// Rescan a visible directory.
    Rescan {
        node: NodeRef,
    },
    /// Rescan the root of a tree loaded from a file ([`Engine::load`](crate::Engine::load))
    /// with these options, which no scan job of this session holds. With `changed` (paths
    /// relative to the root, as [`changed_since`](crate::changed_since) gives them), the tree
    /// is classified as loaded and only those directories are rescanned.
    Resume {
        node: NodeRef,
        opts: JobOptions,
        changed: Option<Vec<PathBuf>>,
    },
    Cancel {
        job: JobId,
    },
    CancelAll,
    /// Delete these visible nodes. Plans run one at a time.
    Delete {
        items: Vec<NodeRef>,
        mode: Mode,
    },
    /// Put back what this plan moved to the trash or staged ([`Status::undo`]).
    Undo {
        plan: u64,
    },
    /// Delete staged items now rather than when their undo window ends.
    CommitStaged,
    /// Run the native command of the rule that matched `node` (its `variant`, if given), or its
    /// dry run; progress goes to [`Status::native`] with `id`. The node's directory is rescanned
    /// after a real run.
    RunNative {
        id: u64,
        node: NodeRef,
        variant: Option<usize>,
        dry_run: bool,
    },
    /// Estimate what deleting these nodes would free; the answer goes to [`Status::estimate`]
    /// with `id`.
    Estimate {
        id: u64,
        items: Vec<NodeRef>,
    },
    /// Drop queued delete plans. The runtime also stops the running plan after its current item.
    AbortDelete,
    /// Panics inside the applier, to test crash handling.
    #[cfg(feature = "fault-injection")]
    InjectPanic,
    /// Give up on a stalled directory.
    Abandon {
        node: NodeRef,
    },
    AbandonAllStalled,
    /// Run the native `size_command` of every rule that matched something.
    NativeSizes,
}

struct Job {
    id: JobId,
    kind: JobKind,
    target: NodeRef,
    root: NodeRef,
    scope: Scope,
    opts: JobOptions,
    map: Pages<Slot>,
    held_close: HashMap<u32, HeldClose>,
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
    /// A rescan that has not swapped in yet: it still blocks overlapping rescans and deletes,
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
    /// Set for a remote scan: scanner calls go to the agent instead of `scan`.
    pub(crate) remote: Option<Arc<crate::remote::Link>>,
    jobs: BTreeMap<u32, Job>,
    /// Options of finished jobs dropped from `jobs`, for rescans of what they scanned.
    retired: HashMap<u32, JobOptions>,
    next_job: u32,
    links: LinkTable,
    work: VecDeque<Work>,
    retry_abandon: Vec<(JobId, DirId)>,
    queued_plans: VecDeque<Plan>,
    active_plan: Option<ActivePlan>,
    outgoing_plans: Vec<Plan>,
    /// Undo and commit requests for the deleter.
    outgoing_controls: Vec<DeleterReq>,
    /// Plans with items that can still be put back: mode, items, bytes, staged items among them.
    undoable: BTreeMap<u64, (Mode, usize, u64)>,
    /// Paths whose directories are rescanned when the running undo or native command ends:
    /// what it put back or what it cleaned.
    rescan_paths: Vec<PathBuf>,
    next_plan: u64,
    /// Rescans needed after a delete that are blocked by the running plan; retried as it runs.
    deferred_rescans: Vec<NodeRef>,
    /// Refuse every delete request.
    pub read_only: bool,
    /// The tree was loaded from a file: refuse what would touch the filesystem.
    pub imported: bool,
    /// Measure extents after each job.
    pub measure: bool,
    /// Files allocating less are not measured ([`MEASURE_MIN`] in the engine).
    pub measure_min: u64,
    /// The deleter runs as root, so read-only btrfs snapshots can be deleted.
    pub privileged: bool,
    extents: ExtentTable,
    /// Devices whose filesystem can share extents, from the scanner's probes.
    extent_devs: HashSet<u64>,
    measure_stack: Vec<NodeRef>,
    outgoing_measure: Vec<MeasureReq>,
    /// Requests for the classifier pool, taken by the runtime after each slice.
    outgoing_checks: Vec<CheckReq>,
    /// A classifier pool answers them; without one nothing is queued.
    pub checks: bool,
    measure_inflight: usize,
    /// Results of earlier rounds (abandoned after a timeout) are ignored.
    measure_round: u64,
    measure_last: u64,
}

/// Files in measurement requests in flight at most.
const MEASURE_INFLIGHT: usize = 32768;
/// Files per measurement request (one directory per request).
const MEASURE_BATCH: usize = 256;
/// Smallest allocation the engine measures on macOS: on APFS each file costs a lookup of its
/// private size, and the clones that matter are large. Elsewhere every file with data is.
pub const MEASURE_MIN: u64 = if cfg!(target_os = "macos") {
    1 << 20
} else {
    1
};
/// A measurement round without any result for this long is abandoned.
const MEASURE_TIMEOUT_NS: u64 = 60_000_000_000;

struct ActivePlan {
    id: u64,
    mode: Mode,
    total: usize,
    remaining: HashSet<NodeRef>,
    summary: DeleteSummary,
}

/// Units of incremental work per step (nodes freed, nodes walked).
const STEP_UNITS: usize = 4096;

/// Hard links examined per step of re-election or merging: whole inodes are taken until this
/// many of their links have been looked at, since re-electing compares the paths of all of an
/// inode's links. 128 keep a step well under the 1 ms write-lock slice on a 10M-entry tree,
/// where each link's walk up the tree misses the cache.
const LINKS_PER_STEP: usize = 128;

pub(crate) fn meta_of(tree: &Tree, ix: Ix) -> Meta {
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

/// What a rescan of directory `ix` classifies against: the rules decided above it, whether it
/// is protected, and its own hits, from its parent's listing as the tree has it. `job` is what
/// the job that scanned `ix` classified against, and `job_root` that job's root.
fn rescan_class(tree: &Tree, ix: Ix, job_root: Option<Ix>, job: &ClassOpts) -> ClassOpts {
    let rules = job.rules.clone();
    // The rule path: the job root's, then the names below it.
    let mut names = Vec::new();
    let mut cur = Some(ix);
    while let Some(i) = cur
        && Some(i) != job_root
    {
        names.push(tree.name(i));
        cur = tree.parent(i);
    }
    let path_of = |names: &[&[u8]]| {
        let mut p = job.path.clone();
        for n in names.iter().rev() {
            if !p.ends_with(b"/") {
                p.push(b'/');
            }
            p.extend_from_slice(n);
        }
        p
    };
    let path = if cur.is_some() {
        path_of(&names)
    } else {
        dfdu_scan::classify::rule_path(&tree.path_buf(ix))
    };
    let mut inside = Vec::new();
    let mut cur = tree.parent(ix);
    while let Some(p) = cur {
        inside.extend(tree.class(p).map(|c| c.rule));
        cur = tree.parent(p);
    }
    inside.reverse();
    let protection = |i: Ix| {
        if tree.is_protected(i) {
            dfdu_rules::protected::State::On
        } else {
            dfdu_rules::protected::State::Off
        }
    };
    let mut chain = Vec::new();
    if let Some(p) = tree.parent(ix) {
        let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
        let parent = if cut == 0 {
            b"/".to_vec()
        } else {
            path[..cut].to_vec()
        };
        let sibling = |n: &[u8]| tree.children(p).any(|c| tree.name(c) == n);
        let v = rules.classify(&dfdu_rules::Candidate {
            parent: &parent,
            name: tree.name(ix),
            is_dir: true,
            sibling: &sibling,
            inside: &inside,
            protection: protection(p),
            partial: false,
            stat: None,
            link: None,
        });
        if let dfdu_rules::Verdict::Hits(h) = v {
            chain = h;
        }
    }
    ClassOpts {
        rules,
        path,
        inside,
        protection: protection(ix),
        chain,
    }
}

/// More changed directories than this and a resume rescans the whole tree.
const MAX_RESUME_RESCANS: usize = 10_000;

/// The directories of the tree at `root` that `paths` (relative to it) name, or the nearest
/// existing ancestor of one not in the tree.
fn changed_dirs(tree: &Tree, root: Ix, paths: &[PathBuf]) -> Vec<Ix> {
    let mut out = Vec::new();
    for p in paths {
        let mut at = root;
        for c in p.components() {
            let std::path::Component::Normal(n) = c else {
                continue;
            };
            let n = dfdu_tree::name_bytes(n);
            match tree
                .children(at)
                .find(|&k| tree.dir(k).is_some() && tree.name(k) == &n[..])
            {
                Some(k) => at = k,
                None => break,
            }
        }
        out.push(at);
    }
    out
}

/// `dirs` without duplicates and without those below another of them.
fn topmost(tree: &Tree, mut dirs: Vec<Ix>) -> Vec<Ix> {
    dirs.sort_unstable_by_key(|d| d.0);
    dirs.dedup();
    let set: HashSet<Ix> = dirs.iter().copied().collect();
    dirs.retain(|&d| {
        let mut cur = tree.parent(d);
        while let Some(p) = cur {
            if set.contains(&p) {
                return false;
            }
            cur = tree.parent(p);
        }
        true
    });
    dirs
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
            retired: HashMap::new(),
            remote: None,
            next_job: 1,
            links: LinkTable::default(),
            work: VecDeque::new(),
            retry_abandon: Vec::new(),
            queued_plans: VecDeque::new(),
            active_plan: None,
            outgoing_plans: Vec::new(),
            outgoing_controls: Vec::new(),
            undoable: BTreeMap::new(),
            rescan_paths: Vec::new(),
            next_plan: 1,
            deferred_rescans: Vec::new(),
            read_only: false,
            imported: false,
            measure: true,
            measure_min: 1,
            privileged: dfdu_sys::is_privileged(),
            extents: ExtentTable::default(),
            extent_devs: HashSet::new(),
            measure_stack: Vec::new(),
            outgoing_measure: Vec::new(),
            outgoing_checks: Vec::new(),
            checks: false,
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

    pub fn command(&mut self, m: &mut Model, cmd: Command, _now: u64) {
        m.recent.push(Event::of_command(&cmd));
        if self.imported
            && matches!(
                cmd,
                Command::Scan { .. }
                    | Command::Rescan { .. }
                    | Command::Resume { .. }
                    | Command::Delete { .. }
                    | Command::RunNative { .. }
                    | Command::NativeSizes
            )
        {
            m.status
                .notice("an imported scan is read-only: nothing is scanned or deleted".into());
            m.status.commands_applied += 1;
            return;
        }
        if m.status.disconnected.is_some()
            && matches!(
                cmd,
                Command::Scan { .. } | Command::Rescan { .. } | Command::Resume { .. }
            )
        {
            m.status
                .notice("the link to the agent is gone: nothing more is scanned".into());
            m.status.commands_applied += 1;
            return;
        }
        match cmd {
            Command::Scan { path, opts } => {
                let id = self.next_job;
                self.next_job += 1;
                let name = dfdu_tree::name_bytes(path.as_os_str()).into_owned();
                let root = m.tree.new_root(&name, &empty_dir_meta(), true);
                if let Some(c) = &opts.class {
                    m.rules = Some(c.rules.clone());
                    m.tree.set_protection(root, c.protection.protected(), false);
                }
                self.start(m, id, JobKind::Initial, root, root, VISIBLE, path, opts);
            }
            Command::Rescan { node } => self.rescan(m, node, None),
            Command::Resume {
                node,
                opts,
                changed: None,
            } => self.rescan(m, node, Some(opts)),
            Command::Resume {
                node,
                opts,
                changed: Some(dirs),
            } => self.resume_changed(m, node, opts, &dirs),
            Command::Cancel { job } => self.cancel(job.0),
            Command::CancelAll => {
                let ids: Vec<u32> = self.jobs.keys().copied().collect();
                for id in ids {
                    self.cancel(id);
                }
            }
            Command::Abandon { node } => self.abandon(m, node),
            Command::AbandonAllStalled => {
                for s in m.status.stalled.clone() {
                    self.abandon(m, s.node);
                }
            }
            Command::Delete { items, mode } => self.submit_plan(m, items, mode),
            Command::Undo { plan } => {
                if m.status.undoing.is_none() && self.undoable.remove(&plan).is_some() {
                    m.status.undoing = Some(plan);
                    self.outgoing_controls.push(DeleterReq::Undo { plan });
                }
            }
            Command::CommitStaged => self.outgoing_controls.push(DeleterReq::Commit),
            Command::RunNative {
                id,
                node,
                variant,
                dry_run,
            } => self.run_native(m, id, node, variant, dry_run),
            Command::Estimate { id, items } => {
                m.status.estimate = Some(self.estimate(m, &items, id));
            }
            Command::NativeSizes => self.native_sizes(m),
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
        job.submitted_seq = match &self.remote {
            Some(l) => l.start_job(JobId(id), &path, &opts),
            None => self.scan.start_job(JobId(id), &path, opts),
        };
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

    /// Rescans `node` with the options of the job that scanned it, or `given`.
    fn rescan(&mut self, m: &mut Model, node: NodeRef, given: Option<JobOptions>) {
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
        // Overlap with deletes and other rescans.
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
            self.cancel(id);
        }
        self.cut(m, ix, DirState::Superseded);
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
        let owner_id = m.tree.dir(ix).expect("dir").job;
        let owner = self.jobs.get(&owner_id);
        let owner_root = owner.and_then(|j| m.tree.resolve(j.root));
        let resumed = given.is_some();
        let mut opts = given
            .or_else(|| owner.map(|j| j.opts.clone()))
            .or_else(|| self.retired.get(&owner_id).cloned())
            .unwrap_or_default();
        // Duplicate mounts are judged against the whole visible tree, not just this part.
        let mut top = ix;
        while let Some(p) = m.tree.parent(top) {
            top = p;
        }
        opts.scope = Some(m.tree.path_buf(top));
        if let Some(c) = opts.class.as_ref().filter(|_| resumed) {
            m.rules = Some(c.rules.clone());
            m.tree
                .set_protection(shadow, c.protection.protected(), false);
        } else if let Some(c) = &mut opts.class {
            *c = rescan_class(&m.tree, ix, owner_root, c);
            let f = m.tree.flags(ix);
            m.tree.set_protection(
                shadow,
                f.has(NodeFlags::PROTECTED),
                f.has(NodeFlags::EXCEPTED),
            );
        }
        self.start(m, id, JobKind::Rescan, ix, shadow, id, path, opts);
    }

    /// Takes over a tree loaded from a snapshot when the volume's change journal says which
    /// directories changed since (`changed`, relative to the root): the tree is classified as
    /// it stands, held by a finished job with `opts`, and only the topmost changed directories
    /// are rescanned, each with its subtree. A changed root, or too many changes, rescan it all.
    fn resume_changed(
        &mut self,
        m: &mut Model,
        node: NodeRef,
        opts: JobOptions,
        changed: &[PathBuf],
    ) {
        let Some(root) = m
            .tree
            .resolve_visible(node)
            .filter(|&r| m.tree.dir(r).is_some() && m.tree.parent(r).is_none())
        else {
            return self.rescan(m, node, Some(opts));
        };
        let mut targets = changed_dirs(&m.tree, root, changed);
        if targets.len() > MAX_RESUME_RESCANS || targets.contains(&root) {
            return self.rescan(m, node, Some(opts));
        }
        let id = self.next_job;
        self.next_job += 1;
        // Other volumes mounted below keep their own journals: their mount roots are rescanned.
        // So are btrfs subvolumes below (another device, no mount): each has a tree, and a
        // generation, of its own.
        let mut mounts = Vec::new();
        let mut own = |tree: &mut Tree, ix: Ix| {
            if let Some(d) = tree.dir_mut(ix) {
                d.job = id;
                d.dir_id = 0;
            }
            let other_dev = || {
                tree.parent(ix)
                    .is_some_and(|p| tree.dev_ino(p).0 != tree.dev_ino(ix).0)
            };
            if ix != root
                && (tree.flags(ix).has(NodeFlags::MOUNT_ROOT) || other_dev())
                && tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped)
            {
                mounts.push(ix);
            }
        };
        match &opts.class {
            Some(c) => {
                m.rules = Some(c.rules.clone());
                m.tree.set_protection(root, c.protection.protected(), false);
                crate::reclassify::classify_loaded(
                    &mut m.tree,
                    root,
                    c,
                    &self.scan.backend,
                    &mut own,
                );
            }
            None => {
                let mut stack = vec![root];
                while let Some(ix) = stack.pop() {
                    own(&mut m.tree, ix);
                    stack.extend(m.tree.children(ix).filter(|&c| m.tree.dir(c).is_some()));
                }
            }
        }
        // Snapshots keep no measured extents: the loaded files are measured again where files
        // can share them (rescanned parts are measured when their rescans end).
        let mut devs = vec![root];
        devs.extend(mounts.iter().copied());
        for ix in devs {
            let dev = m.tree.dev_ino(ix).0;
            if !self.extent_devs.contains(&dev) {
                let loc = self.scan.backend.loc_from_path(&m.tree.path_buf(ix));
                if self.scan.backend.shares_extents(&loc) {
                    self.extent_devs.insert(dev);
                }
            }
        }
        self.start_measure(m, node);
        targets.extend(mounts);
        let targets = topmost(&m.tree, targets);
        self.jobs.insert(
            id,
            Job {
                id: JobId(id),
                kind: JobKind::Initial,
                target: node,
                root: node,
                scope: VISIBLE,
                opts,
                map: Pages::default(),
                held_close: HashMap::new(),
                stalled: HashMap::new(),
                submitted_seq: 0,
                phase: JobPhase::Done,
            },
        );
        m.tree.bump_version();
        self.queue_checks(m);
        for t in targets {
            let r = m.tree.node_ref(t);
            self.rescan(m, r, None);
        }
    }

    /// Cuts a directory its job is still scanning: its job's tasks for it are stopped, and it
    /// reports to its parent at once with `state`. Does nothing to a directory that is not
    /// pending or already cut.
    fn cut(&mut self, m: &mut Model, ix: Ix, state: DirState) {
        let Some(rec) = m.tree.dir_mut(ix) else {
            return;
        };
        if rec.state != DirState::Pending || rec.cut {
            return;
        }
        rec.cut = true;
        rec.state = state;
        rec.outcome = state;
        let (job, dir) = (rec.job, rec.dir_id);
        if !rec.reported {
            rec.reported = true;
            self.report_up(m, job, ix);
        }
        m.tree.touch(ix);
        match &self.remote {
            Some(l) => l.stop_dirs(JobId(job), vec![DirId(dir)]),
            None => self.scan.stop_dirs(JobId(job), [DirId(dir)]),
        }
    }

    fn cancel(&mut self, id: u32) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        if j.phase != JobPhase::Running {
            return;
        }
        j.phase = JobPhase::Cancelled;
        match &self.remote {
            Some(l) => l.end_job(j.id),
            None => self.scan.end_job(j.id),
        }
        let root = j.root;
        self.work.push_back(Work::CancelWalk {
            job: id,
            stack: vec![root],
        });
        self.work.push_back(Work::FinishJob { job: id });
    }

    /// Ends a remote scan whose link ended, once everything that arrived before is applied:
    /// running jobs stop where they are, which leaves the scan incomplete.
    pub(crate) fn check_link(&mut self, m: &mut Model) {
        let Some(why) = self.remote.as_ref().and_then(|l| l.take_lost()) else {
            return;
        };
        let running: Vec<u32> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.phase == JobPhase::Running)
            .map(|(&id, _)| id)
            .collect();
        if !running.is_empty() {
            m.status.incomplete = true;
        }
        for id in running {
            self.cancel(id);
        }
        m.status.notice(format!("remote scan: {why}"));
        m.status.disconnected = Some(why);
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
        self.scanner_abandon(JobId(job), DirId(dir));
    }

    fn scanner_abandon(&mut self, job: JobId, dir: DirId) {
        let complete = match &self.remote {
            Some(l) => l.abandon(job, dir),
            None => self.scan.abandon(job, dir),
        };
        if !complete {
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
                classes,
            } => self.apply_close(m, job, dir, chunks, (status, classes)),
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

    /// Returns the directory node if the message should be applied. A message for a directory
    /// that is gone or cut also stops its tasks and those of the chunk's subdirectories.
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
        if let Some(ix) = r.and_then(|r| m.tree.resolve_uncut(r)) {
            return Some(ix);
        }
        // Gone, detached, or under a cut.
        st.discarded_cut += 1;
        let mut stop = vec![dir];
        if let Some((first, n)) = subdirs {
            stop.extend((0..n as u32).map(|i| DirId(first.0 + i)));
        }
        let j = self.jobs.get_mut(&job.0).expect("checked above");
        for &d in &stop[1..] {
            j.put(d, Slot::Dead);
        }
        match &self.remote {
            Some(l) => l.stop_dirs(job, stop.into_iter().collect()),
            None => self.scan.stop_dirs(job, stop),
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
            // From a part that returned after the directory was abandoned. Its subdirectories
            // were never added, but tasks for them exist: stop them, and drop what they send
            // (as under a cut).
            m.status.stats.late += 1;
            let ids: Vec<DirId> = (0..c.subdirs.len() as u32)
                .map(|i| DirId(c.first_subdir.0 + i))
                .collect();
            let j = self.jobs.get_mut(&c.job.0).expect("gated job");
            for &d in &ids {
                j.put(d, Slot::Dead);
            }
            match &self.remote {
                Some(l) => l.stop_dirs(c.job, ids),
                None => self.scan.stop_dirs(c.job, ids),
            }
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
        // Nodes of the entries that have marks, by entry, only when some have.
        let mut file_ix = Vec::new();
        let mut subdir_ix = Vec::new();
        let marked = !c.marks.is_empty();
        for f in &c.files {
            let name = c.name(f.name);
            let fx = if f.meta.is_hardlink() {
                let mut fx = Ix(0);
                self.links
                    .add_scanned((f.meta.dev, f.meta.ino), f.meta.nlink, scope, |charged| {
                        fx = m.tree.add_file(ix, name, &f.meta, charged, None);
                        m.tree.node_ref(fx)
                    });
                fx
            } else {
                m.tree.add_file(ix, name, &f.meta, false, None)
            };
            if marked {
                file_ix.push(fx);
            }
            delta.add(&m.tree.own(fx));
        }
        for e in &c.errors {
            let meta = Meta {
                kind: Kind::Other,
                ..empty_dir_meta()
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
            if marked {
                subdir_ix.push(cx);
            }
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
        for (e, mark) in &c.marks {
            let node = match *e {
                Entry::File(i) => file_ix.get(i as usize),
                Entry::Subdir(i) => subdir_ix.get(i as usize),
            };
            let Some(&node) = node else {
                m.status.stats.corrupt += 1;
                continue;
            };
            match *mark {
                Mark::Class(class) => m.tree.set_class(node, Some(class)),
                Mark::Protected => m.tree.set_protection(node, true, false),
                Mark::Excepted => m.tree.set_protection(node, false, true),
            }
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
        status: (ListingStatus, Vec<(DirId, Class)>),
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

    fn close_with_status(
        &mut self,
        m: &mut Model,
        job: u32,
        ix: Ix,
        (status, classes): (ListingStatus, Vec<(DirId, Class)>),
    ) {
        // Every chunk is in, so the subdirectories the classes name have nodes.
        if let Some(j) = self.jobs.get(&job) {
            for (d, c) in classes {
                if let Some(cx) = j.get(d).and_then(|r| m.tree.resolve(r)) {
                    m.tree.set_class(cx, Some(c));
                }
            }
        }
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

    /// Closes the directory's own listing and decrements its pending count.
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
        self.dec_pending(m, job, ix);
    }

    /// Decrements `ix`'s pending count and cascades completion upward.
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
        match &self.remote {
            Some(l) => l.end_job(j.id),
            None => self.scan.end_job(j.id),
        }
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
        // per-directory totals never depend on scan order.
        self.work.push_back(Work::FinalPassVisible);
        self.queue_checks(m);
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
                if let Some(j) = self.jobs.remove(&k) {
                    self.retired.insert(k, j.opts);
                }
            }
        }
    }

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
                    || tree.raw_size(c).1 < self.measure_min.max(1)
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

    /// Requests for the classifier pool.
    pub fn take_check_requests(&mut self) -> Vec<CheckReq> {
        std::mem::take(&mut self.outgoing_checks)
    }

    /// Queues the guard checks of every visible class not checked or queued yet. Classes are
    /// few (one per match, nothing below it), so this walks all of them.
    fn queue_checks(&mut self, m: &mut Model) {
        let Some(rules) = m.rules.clone().filter(|_| self.checks) else {
            return;
        };
        let todo: Vec<(Ix, Class)> = m
            .tree
            .classes()
            .filter(|(ix, c)| !c.has(Class::QUEUED | Class::CHECKED) && m.tree.is_visible(*ix))
            .collect();
        for (ix, mut c) in todo {
            let g = &rules.rule(c.rule).guard;
            if !g.not_vcs_tracked && g.not_running.is_empty() {
                continue;
            }
            self.outgoing_checks.push(CheckReq::Guards {
                node: m.tree.node_ref(ix),
                rule: c.rule,
                path: m.tree.path_buf(ix),
                vcs: g.not_vcs_tracked,
                processes: g.not_running.clone(),
            });
            c.flags |= Class::QUEUED;
            m.tree.set_class(ix, Some(c));
        }
    }

    fn native_sizes(&mut self, m: &mut Model) {
        let Some(rules) = m.rules.clone().filter(|_| self.checks) else {
            return;
        };
        let mut matched: Vec<u16> = m.tree.classes().map(|(_, c)| c.rule).collect();
        matched.sort_unstable();
        matched.dedup();
        for rule in matched {
            if let Some(n) = &rules.rule(rule).native
                && !n.size_command.is_empty()
            {
                self.outgoing_checks.push(CheckReq::NativeSize {
                    rule,
                    check: n.check.clone(),
                    command: n.size_command.clone(),
                });
            }
        }
    }

    /// Applies an answer of the classifier pool. One for a node freed since, or whose class
    /// changed (a rescan), is dropped.
    pub fn apply_check(&mut self, m: &mut Model, msg: CheckMsg) {
        match msg {
            CheckMsg::Guards {
                node,
                rule,
                tracked,
                running,
            } => {
                let Some(ix) = m.tree.resolve(node) else {
                    return;
                };
                let Some(mut c) = m.tree.class(ix).filter(|c| c.rule == rule) else {
                    return;
                };
                c.flags = (c.flags & !Class::QUEUED) | Class::CHECKED;
                match tracked {
                    Some(true) => {
                        c.flags |= Class::VCS_TRACKED;
                        c.safety = dfdu_rules::Safety::Protected as u8;
                    }
                    Some(false) => {}
                    None => c.flags |= Class::VCS_UNKNOWN,
                }
                if running.is_some() {
                    c.flags |= Class::IN_USE;
                }
                m.tree.set_class(ix, Some(c));
                m.tree.touch(ix);
            }
            CheckMsg::NativeSize { rule, result } => {
                m.native_sizes.insert(rule, result);
                m.tree.bump_version();
            }
        }
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
        // Only the part of `refresh_status` a result can change: all of it per result costs a
        // tenth of the applier's time while measuring.
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

    /// Plans for the deleter thread, taken by the runtime after each slice.
    pub fn take_plans(&mut self) -> Vec<Plan> {
        std::mem::take(&mut self.outgoing_plans)
    }

    /// Undo and commit requests for the deleter thread, sent after the plans.
    pub fn take_deleter_controls(&mut self) -> Vec<DeleterReq> {
        std::mem::take(&mut self.outgoing_controls)
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

    /// Why the tree says `ix` cannot be deleted; the deleter checks the filesystem again
    /// before touching it.
    fn delete_refusal(tree: &Tree, ix: Ix, privileged: bool) -> Option<String> {
        let flags = tree.flags(ix);
        let t = tree.total(ix);
        if flags.has(NodeFlags::MOUNT_ROOT) {
            return Some("is a mount point; unmount it first".into());
        }
        if tree.is_protected(ix) {
            return Some("is protected (version control, secrets or system data)".into());
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
        if t.ro_subvols > 0 && !privileged {
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
            // Overlap with rescans: a running rescan of the item, an ancestor or a
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
            if let Some(why) = Self::delete_refusal(&m.tree, ix, self.privileged) {
                m.status.notice(format!(
                    "delete: {} {why}",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            let inside = std::sync::Arc::new(listings(&m.tree, ix));
            self.cut(m, ix, DirState::Deleting);
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
                inside,
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
                mode: plan.mode,
                total: remaining.len(),
                remaining,
                summary: DeleteSummary {
                    plan: plan.id,
                    mode: Some(plan.mode),
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
                Some(_) => self.rescan(m, r, None),
            }
        }
    }

    /// Applies a result from the deleter.
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
                        if active.mode.undoable() {
                            let u = self.undoable.entry(plan).or_insert((active.mode, 0, 0));
                            u.1 += 1;
                            u.2 += bytes;
                        }
                        if active.mode == Mode::Stage {
                            m.status.staged.0 += 1;
                            m.status.staged.1 += bytes;
                        }
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
                            rescan = (was_cut || partial) && !skipped && m.tree.dir(ix).is_some();
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
                        if s.problems.len() < PROBLEMS_KEPT {
                            s.problems.push((node, other.to_string()));
                        }
                    }
                }
                let done = active.total - active.remaining.len();
                if let Some(d) = m.status.deleting.as_mut() {
                    d.done = done;
                }
                if rescan {
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
            DeleterMsg::Committed {
                plan,
                bytes,
                result,
            } => {
                m.status.staged.0 = m.status.staged.0.saturating_sub(1);
                m.status.staged.1 = m.status.staged.1.saturating_sub(bytes);
                self.forget_undoable(plan, bytes);
                if let Err(e) = result {
                    m.status.notice(format!("delete staged: {e}"));
                }
            }
            DeleterMsg::Restored {
                plan: _,
                original,
                staged,
                bytes,
                result,
            } => {
                if staged {
                    m.status.staged.0 = m.status.staged.0.saturating_sub(1);
                    m.status.staged.1 = m.status.staged.1.saturating_sub(bytes);
                }
                match result {
                    Ok(to) => {
                        if to != original {
                            m.status.notice(format!(
                                "undo: {} exists again; restored as {}",
                                original.display(),
                                to.display()
                            ));
                        }
                        self.rescan_paths.push(to);
                    }
                    Err(e) => m.status.notice(format!("undo {}: {e}", original.display())),
                }
            }
            DeleterMsg::NativeOutput { id, line } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    if n.lines.len() == NATIVE_LINES {
                        n.lines.pop_front();
                    }
                    n.lines.push_back(line);
                }
            }
            DeleterMsg::NativeDone { id, result } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    let rescan = !n.dry_run && result.is_ok();
                    n.done = Some(result);
                    if rescan {
                        self.rescan_paths(m);
                    }
                }
                self.rescan_paths.clear();
            }
            DeleterMsg::UndoDone { plan } => {
                if m.status.undoing == Some(plan) {
                    m.status.undoing = None;
                }
                self.rescan_paths(m);
            }
            DeleterMsg::Unfinished { items, notes } => {
                if let Some(first) = items.first() {
                    let more = match items.len() {
                        1 => String::new(),
                        n => format!("; and {} more", n - 1),
                    };
                    m.status.notice(format!(
                        "an earlier session left {} delete{} unfinished, kept as they are \
                         (dfdu recover lists them): {first}{more}",
                        items.len(),
                        if items.len() == 1 { "" } else { "s" },
                    ));
                }
                for n in notes {
                    m.status.notice(format!("deletion journal: {n}"));
                }
            }
        }
        self.refresh_undo(m);
        self.refresh_status(m);
    }

    fn run_native(
        &mut self,
        m: &mut Model,
        id: u64,
        node: NodeRef,
        variant: Option<usize>,
        dry_run: bool,
    ) {
        if self.read_only && !dry_run {
            m.status.notice("native command: read-only mode".into());
            return;
        }
        let (Some(rules), Some(ix)) = (m.rules.clone(), m.tree.resolve_visible(node)) else {
            return;
        };
        let Some(class) = m.tree.class(ix) else {
            return;
        };
        let path = m.tree.path_buf(ix);
        let Some(req) = variant
            .map_or(Some(None), |v| u16::try_from(v).ok().map(Some))
            .and_then(|v| native_req(&rules, class.rule, v, &path, dry_run, id))
        else {
            return;
        };
        m.status.native = Some(NativeRun {
            id,
            rule: class.rule,
            command: dfdu_clean::shown_argv(&req.command),
            cwd: req.cwd.clone(),
            dry_run,
            lines: VecDeque::new(),
            done: None,
        });
        self.rescan_paths.clear();
        if !dry_run {
            self.rescan_paths.push(path);
        }
        self.outgoing_controls.push(DeleterReq::Native(req));
    }

    /// What deleting `items` would free. Walks them, so it is linear in their size.
    pub fn estimate(&self, m: &Model, items: &[NodeRef], id: u64) -> Estimate {
        let tree = &m.tree;
        let all: HashSet<Ix> = items
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        let tops: Vec<Ix> = all
            .iter()
            .copied()
            .filter(|&ix| {
                let mut a = ix;
                while let Some(p) = tree.parent(a) {
                    if all.contains(&p) {
                        return false;
                    }
                    a = p;
                }
                true
            })
            .collect();
        let mut e = Estimate {
            id,
            items: tops.len(),
            // APFS clones share data without a sign the scan can read.
            may_free_less: cfg!(target_os = "macos"),
            ..Default::default()
        };
        // Hard-linked inodes in the targets: links seen, and the charged link's bytes if it is
        // among them.
        let mut linked: HashMap<Inode, (u32, u64)> = HashMap::new();
        for &t in &tops {
            let tot = tree.total(t);
            e.bytes += tot.alloc;
            e.shared_out += tot.shared_out;
            e.nodes += tot.items;
            e.may_free_less |= self.extent_devs.contains(&tree.dev_ino(t).0) && !self.measure;
            let mut stack = vec![t];
            while let Some(ix) = stack.pop() {
                if let Some(d) = tree.dir(ix) {
                    e.scanning |= d.state == DirState::Pending || d.cut;
                    stack.extend(tree.children(ix));
                    continue;
                }
                let f = tree.flags(ix);
                if f.has(NodeFlags::HARDLINK) {
                    let l = linked.entry(tree.dev_ino(ix)).or_default();
                    l.0 += 1;
                    if f.has(NodeFlags::CHARGED) {
                        l.1 = tree.own(ix).alloc;
                    }
                }
            }
        }
        for (key, (seen, bytes)) in linked {
            if seen < self.links.nlink(key) {
                e.linked_out += bytes;
            }
        }
        e.bytes = e.bytes.saturating_sub(e.shared_out + e.linked_out);
        e
    }

    /// A staged item of `plan` was deleted: it can no longer be put back.
    fn forget_undoable(&mut self, plan: u64, bytes: u64) {
        if let Some(u) = self.undoable.get_mut(&plan) {
            u.1 = u.1.saturating_sub(1);
            u.2 = u.2.saturating_sub(bytes);
            if u.1 == 0 {
                self.undoable.remove(&plan);
            }
        }
    }

    fn refresh_undo(&mut self, m: &mut Model) {
        m.status.undo = self
            .undoable
            .iter()
            .next_back()
            .map(|(&plan, &(mode, items, bytes))| UndoInfo {
                plan,
                mode,
                items,
                bytes,
            });
    }

    /// Rescans the nearest directory in the tree above each item an undo put back: the
    /// highest of them only, since a rescan covers what is below it.
    fn rescan_paths(&mut self, m: &mut Model) {
        let mut dirs: Vec<Ix> = std::mem::take(&mut self.rescan_paths)
            .iter()
            .filter_map(|p| p.parent().and_then(|p| node_at(&m.tree, p)))
            .collect();
        dirs.sort_unstable();
        dirs.dedup();
        let tops: Vec<Ix> = dirs
            .iter()
            .copied()
            .filter(|&d| {
                !dirs
                    .iter()
                    .any(|&a| a != d && Self::is_ancestor(&m.tree, a, d))
            })
            .collect();
        for d in tops {
            self.deferred_rescans.push(m.tree.node_ref(d));
        }
        self.retry_deferred_rescans(m);
    }

    /// The extent table's view of one inode, for debugging.
    pub fn describe_extents(&self, m: &Model, inode: Inode) -> String {
        self.extents.describe(&m.tree, inode)
    }

    /// Checks what must hold once nothing is pending: every measured inode is counted at a
    /// visible node.
    pub fn check_settled(&self, m: &Model) -> Result<(), String> {
        self.extents.check_settled(&m.tree)
    }

    /// Checks the tree's structure and totals, and the pending counts of running jobs. Linear
    /// in tree size.
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
                        "{:?} pending {} expected {} (job {})",
                        m.tree.path_buf(ix).display(),
                        rec.pending,
                        expect,
                        j.id.0
                    ));
                }
                if (rec.pending == 0) != rec.state.is_terminal() {
                    return Err(format!(
                        "pending {} with state {:?}",
                        rec.pending, rec.state
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A rule's native command as it runs for the matched node at `path`.
pub fn native_argv(argv: &[String], path: &Path) -> Vec<String> {
    argv.iter().map(|a| native_var(a, path)).collect()
}

/// The native command of `rule` (its `variant`, if given; the dry run with `dry_run`) for the
/// matched node at `path`, or `None` when the rule has no such command. The agent builds what
/// it is asked to run with this from its own rules, as the local side did.
pub(crate) fn native_req(
    rules: &dfdu_rules::RuleSet,
    rule: u16,
    variant: Option<u16>,
    path: &Path,
    dry_run: bool,
    id: u64,
) -> Option<dfdu_clean::NativeReq> {
    let def = rules.rules().get(rule as usize)?.native.as_ref()?;
    let (command, dry) = match variant {
        None => (&def.command, &def.dry_run),
        Some(v) => {
            let x = def.variants.get(v as usize)?;
            (&x.command, &x.dry_run)
        }
    };
    let argv = if dry_run { dry } else { command };
    if argv.is_empty() {
        return None;
    }
    Some(dfdu_clean::NativeReq {
        id,
        check: def.check.clone(),
        pre: def.pre.clone(),
        command: native_argv(argv, path),
        cwd: def
            .cwd
            .as_deref()
            .map(|c| PathBuf::from(native_var(c, path))),
        timeout: NATIVE_TIMEOUT,
        dry_run,
        rule: Some(dfdu_clean::NativeRule {
            rule,
            variant,
            path: path.to_path_buf(),
        }),
    })
}

/// Expands the variables a native command may use, for the matched node at `path`: `{path}`,
/// `{parent}`, `{name}` and `{name_stem}`.
fn native_var(arg: &str, path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let raw = dfdu_tree::name_bytes(path.file_name().unwrap_or_default());
    let stem = String::from_utf8_lossy(dfdu_rules::stem(&raw));
    arg.replace("{path}", &path.to_string_lossy())
        .replace("{parent}", &path.parent().unwrap_or(path).to_string_lossy())
        .replace("{name_stem}", &stem)
        .replace("{name}", &name)
}

/// What the scan saw in each directory of `item` it read completely, for the deleter to
/// compare (`dfdu_clean::inside`). Linear in the item's size, under the write lock.
fn listings(tree: &Tree, item: Ix) -> dfdu_clean::inside::Listings {
    use dfdu_clean::inside::{Listing, name_hash};
    let mut out = dfdu_clean::inside::Listings::new();
    if tree.kind(item) != Kind::Dir {
        return out;
    }
    let mut stack = vec![(item, PathBuf::new())];
    while let Some((d, rel)) = stack.pop() {
        if tree.dir(d).is_none_or(|r| r.state != DirState::Complete) {
            continue;
        }
        let mut entries = vec![];
        for c in tree.children(d) {
            let name = tree.name(c);
            entries.push((name_hash(name), tree.dev_ino(c).1, tree.kind(c)));
            if tree.kind(c) == Kind::Dir {
                stack.push((c, rel.join(&*dfdu_tree::name_os(name))));
            }
        }
        entries.sort_unstable();
        out.insert(
            rel,
            Listing {
                ino: tree.dev_ino(d).1,
                entries,
            },
        );
    }
    out
}

/// The deepest visible node on `path`: the node itself when the tree has it, else its nearest
/// ancestor that it has.
fn node_at(tree: &Tree, path: &Path) -> Option<Ix> {
    let root = tree
        .roots()
        .filter(|&r| tree.is_visible(r))
        .find(|&r| path.starts_with(tree.path_buf(r)))?;
    let rest = path.strip_prefix(tree.path_buf(root)).ok()?;
    let mut at = root;
    for c in rest.components() {
        let name = dfdu_tree::name_bytes(c.as_os_str());
        match tree.children(at).find(|&ch| tree.name(ch) == &*name) {
            Some(ch) if tree.kind(ch) == Kind::Dir => at = ch,
            _ => break,
        }
    }
    Some(at)
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
