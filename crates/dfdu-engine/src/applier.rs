//! The applier core: gate, chunk and close handling, pending accounting, jobs, cuts, rescans,
//! stalls, quiescence and hard links. No threads: the runtime and the simulation harness both
//! drive it. One `Applier`, its `impl` split by concern over the submodules; this module holds
//! the state, the commands and their dispatch.

mod delete;
mod estimate;
mod guards;
mod invariants;
mod jobs;
mod measure;
mod messages;
mod model;
mod native;
mod rescan;
mod scanner;
#[cfg(test)]
mod test_util;
mod undo;
mod work;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use dfdu_clean::{DeleterReq, Mode, Plan};
use dfdu_scan::{Backend, DirId, JobId, JobOptions, ListingStatus, Shared};
use dfdu_tree::{Class, HostPath, Ix, Kind, Meta, NodeRef, Pages, Tree};

pub use measure::MEASURE_MIN;
pub use model::{
    Count, DeleteProgress, DeleteSummary, Estimate, JobInfo, JobKind, JobPhase, MeasureProgress,
    Model, NATIVE_LINES, NativeRun, PROBLEMS_KEPT, StalledInfo, Stats, Status, Timing, UndoInfo,
};
pub use native::native_argv;
pub(crate) use native::native_req;

use crate::classifier::CheckReq;
use crate::extents::{ExtentTable, Inode};
use crate::links::{LinkTable, Scope, VISIBLE};
use crate::measure::MeasureReq;
use crate::recent::Event;
use delete::ActivePlan;
use jobs::JobStart;
use measure::MeasureAt;
use undo::Undoable;
use work::Work;

/// A `Close` that arrived before some of its directory's chunks: their number, the status, and
/// the classes it carries.
type HeldClose = (u32, (ListingStatus, Vec<(DirId, Class)>));

#[derive(Clone, Debug)]
pub enum Command {
    /// Start a scan of `path` as a new visible root. A path on this machine (a
    /// `PathBuf` converts with `into()`), or for a remote engine the agent's
    /// ([`Connected::root`](crate::Connected::root)).
    Scan {
        path: HostPath,
        opts: JobOptions,
    },
    /// Rescan a visible directory.
    Rescan {
        node: NodeRef,
    },
    /// Scan a mount the scan listed but did not enter (another filesystem, or a network one),
    /// in place, with the options of the job that listed it: the mount is entered, mounts
    /// below it are treated as before. Rescans enter it again until the session ends.
    Enter {
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
    /// Directories it scanned were queued for extent measurement as their listings closed.
    measures: bool,
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
    /// The node of `d`, when it has one.
    fn live(&self, d: DirId) -> Option<NodeRef> {
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
    fn set_live(&mut self, d: DirId, r: NodeRef) {
        self.put(d, Slot::Live(r));
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent settings of the session, not the states of one machine"
)]
pub struct Applier<B: Backend> {
    /// The local scanner. Its backend serves local reads; its jobs run only when `remote` is
    /// unset, and are steered through the `scanner_*` methods, which pick one of the two.
    pub scan: Arc<Shared<B>>,
    /// Set for a remote scan: the agent that runs its jobs.
    pub(crate) remote: Option<Arc<crate::remote::Link>>,
    jobs: BTreeMap<u32, Job>,
    /// Options of finished jobs dropped from `jobs`, for rescans of what they scanned.
    retired: HashMap<u32, JobOptions>,
    /// Mounts the user entered this session ([`Command::Enter`]), by the device and inode of
    /// their roots: every rescan enters them again.
    entered: Vec<(u64, u64)>,
    next_job: u32,
    links: LinkTable,
    work: VecDeque<Work>,
    retry_abandon: Vec<(JobId, DirId)>,
    queued_plans: VecDeque<Plan>,
    active_plan: Option<ActivePlan>,
    outgoing_plans: Vec<Plan>,
    /// Undo and commit requests for the deleter.
    outgoing_controls: Vec<DeleterReq>,
    /// Plans with items that can still be put back, by plan.
    undoable: BTreeMap<u64, Undoable>,
    /// Paths whose directories are rescanned when the running undo or native command ends:
    /// what it put back or what it cleaned.
    rescan_paths: Vec<dfdu_tree::HostPath>,
    next_plan: u64,
    /// Rescans needed after a delete that are blocked by the running plan; retried as it runs.
    deferred_rescans: Vec<NodeRef>,
    /// Rescans waiting for their parent's listing to close, and whether each enters a mount:
    /// a directory's class can depend on its siblings, which a listing still being read does
    /// not have yet.
    rescans_after_listing: Vec<(NodeRef, bool)>,
    /// Refuse every delete request and native command (dry runs aside).
    pub read_only: bool,
    /// The tree was loaded from a file: refuse everything that would touch the filesystem.
    pub(crate) imported: bool,
    /// Measure extents: each directory of a scan as its listing closes, and what is left of a
    /// job's tree when it ends.
    pub measure: bool,
    /// Files allocating less are not measured (`MEASURE_MIN` in the engine).
    pub measure_min: u64,
    /// The deleter runs as root, so read-only btrfs snapshots can be deleted.
    pub privileged: bool,
    extents: ExtentTable,
    /// Devices whose filesystem can share extents, from the scanner's probes.
    extent_devs: HashSet<u64>,
    measure_stack: Vec<MeasureAt>,
    /// The node each inode with a measurement in flight was requested for.
    measure_awaited: HashMap<Inode, NodeRef>,
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
            entered: Vec::new(),
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
            rescans_after_listing: Vec::new(),
            read_only: false,
            imported: false,
            measure: true,
            measure_min: 1,
            privileged: dfdu_sys::is_privileged(),
            extents: ExtentTable::default(),
            extent_devs: HashSet::new(),
            measure_stack: Vec::new(),
            measure_awaited: HashMap::new(),
            outgoing_measure: Vec::new(),
            outgoing_checks: Vec::new(),
            checks: false,
            measure_inflight: 0,
            measure_round: 0,
            measure_last: 0,
        }
    }

    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.work.is_empty() || self.measure_ready()
    }

    /// Roots of running rescan shadows, for the structural invariant check.
    #[must_use]
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
                if let Some(r) = j.live(DirId(d)) {
                    stalled.push(StalledInfo {
                        node: r,
                        since_ns: since,
                    });
                }
            }
        }
        m.status.stalled = stalled;
        m.status.settling = !self.work.is_empty();
        if self.measure_idle() {
            m.status.measuring = None;
        }
    }

    /// Starts a scan of `path` into a new visible root.
    fn start_scan(&mut self, m: &mut Model, path: HostPath, opts: JobOptions) {
        let id = self.next_job;
        self.next_job += 1;
        let root = m.tree.new_root(path.as_bytes(), &empty_dir_meta(), true);
        if let Some(c) = &opts.class {
            m.rules = Some(c.rules.clone());
            m.tree.set_protection(root, c.protection.protected(), false);
        }
        let start = JobStart {
            kind: JobKind::Initial,
            target: root,
            root,
            scope: VISIBLE,
            path,
            opts,
        };
        self.start(m, id, start);
    }

    /// Applies one command.
    ///
    /// # Panics
    ///
    /// On `Command::InjectPanic` (with the `fault-injection` feature), to test what follows
    /// an applier panic.
    pub fn apply_command(&mut self, m: &mut Model, cmd: Command) {
        m.recent.push(Event::of_command(&cmd));
        if let Some(why) = self.refusal(m, &cmd) {
            m.status.notice(why.into());
            m.status.commands_applied += 1;
            return;
        }
        match cmd {
            Command::Scan { path, opts } => self.start_scan(m, path, opts),
            Command::Rescan { node } => self.rescan(m, node, None, false),
            Command::Enter { node } => self.rescan(m, node, None, true),
            Command::Resume {
                node,
                opts,
                changed: None,
            } => self.rescan(m, node, Some(opts), false),
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
            #[expect(clippy::panic, reason = "the command exists to test the crash path")]
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

    /// Why `cmd` is refused, if it is. The one place that decides what read-only mode, an
    /// imported tree and a lost agent allow: the commands themselves do not check again.
    fn refusal(&self, m: &Model, cmd: &Command) -> Option<&'static str> {
        let scans = matches!(
            cmd,
            Command::Scan { .. }
                | Command::Rescan { .. }
                | Command::Enter { .. }
                | Command::Resume { .. }
        );
        if self.imported
            && (scans
                || matches!(
                    cmd,
                    Command::Delete { .. } | Command::RunNative { .. } | Command::NativeSizes
                ))
        {
            // A native command's dry run is refused too: it would run on this host.
            return Some("an imported scan is read-only: nothing is scanned or deleted");
        }
        if self.read_only {
            match cmd {
                Command::Delete { .. } => return Some("delete: read-only mode"),
                Command::RunNative { dry_run: false, .. } => {
                    return Some("native command: read-only mode");
                }
                _ => {}
            }
        }
        if scans && m.status.disconnected.is_some() {
            return Some("the link to the agent is gone: nothing more is scanned");
        }
        None
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
}
