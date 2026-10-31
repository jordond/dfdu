//! What readers see under the read lock: the model, the engine status and its parts.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use dfdu_clean::Mode;
use dfdu_scan::JobId;
use dfdu_tree::{NodeRef, RuleIx, Subvol, Tree};

use crate::recent::Recent;

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
    /// What each rule's native `size_command` reported, by rule
    /// ([`Command::NativeSizes`](super::Command::NativeSizes)).
    pub native_sizes: HashMap<RuleIx, Result<u64, String>>,
}

impl Model {
    /// Changes whenever anything a reader can see may have changed. The applier wakes readers
    /// only when it changes; readers can compare it to skip redraws. Every field of [`Status`]
    /// but `timing` and the `stats` other than `messages` is in it, some by a summary (a count,
    /// an id) that changes with them. The tree's contents, and new values in
    /// [`Model::native_sizes`], show in the tree's version.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let s = &self.status;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.tree.version().hash(&mut h);
        self.native_sizes.len().hash(&mut h);
        s.stats.messages.hash(&mut h);
        s.commands_applied.hash(&mut h);
        s.notices.len().hash(&mut h);
        s.notices_added.hash(&mut h);
        s.anomalies.len().hash(&mut h);
        s.stalled.len().hash(&mut h);
        s.incomplete.hash(&mut h);
        s.settling.hash(&mut h);
        s.fatal.is_some().hash(&mut h);
        s.disconnected.is_some().hash(&mut h);
        s.unmeasured.hash(&mut h);
        s.compressed_upper_bound.hash(&mut h);
        s.exact_sizes.hash(&mut h);
        s.profile.map(|(p, r)| (p, r.to_bits())).hash(&mut h);
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

    /// The scan below `root` is complete: no entry had an error and nothing was left
    /// unfinished.
    #[must_use]
    pub fn complete(&self, root: dfdu_tree::Ix) -> bool {
        self.tree.total(root).errors == 0 && !self.status.incomplete
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
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "below 4, and bit positions below 64, fit any usize"
    )]
    pub fn bucket(d: std::time::Duration) -> usize {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        if us < 4 {
            return us as usize;
        }
        let e = (u64::BITS - 1 - us.leading_zeros()) as usize;
        let m = ((us >> (e - 2)) & 3) as usize;
        (4 * (e - 1) + m).min(Self::BUCKETS - 1)
    }

    /// The upper bound of bucket `k` (exclusive).
    #[must_use]
    pub fn upper(k: usize) -> std::time::Duration {
        let us = if k < 4 {
            k as u64 + 1
        } else {
            (5 + (k % 4) as u64) << (k / 4 - 1)
        };
        std::time::Duration::from_micros(us)
    }

    /// Records a slice of the applier that held the write lock for `held`, after waiting
    /// `waited` for it.
    pub fn record(&mut self, held: std::time::Duration, waited: std::time::Duration) {
        self.slices += 1;
        self.locked_ns += crate::nanos::nanos(held);
        self.hold[Self::bucket(held)] += 1;
        self.wait[Self::bucket(waited)] += 1;
    }

    /// Upper bound of the `p`-th percentile (0 to 1) of a histogram.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "a fraction (0 to 1) of a count, rounded up: precision past 2^52 does not matter"
    )]
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
    #[must_use]
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the engine shown to readers, not the states of one machine"
)]
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
    /// Notices added so far, so that a new one changes the fingerprint once `notices` is full.
    notices_added: u64,
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
    pub staged: Count,
    /// The latest plan whose items can still be put back
    /// ([`Command::Undo`](super::Command::Undo)).
    pub undo: Option<UndoInfo>,
    /// An undo is running for this plan.
    pub undoing: Option<u64>,
    /// The answer to the latest [`Command::Estimate`](super::Command::Estimate).
    pub estimate: Option<Estimate>,
    /// The latest native cleanup command
    /// ([`Command::RunNative`](super::Command::RunNative)).
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
    pub profile: Option<(dfdu_sys::Profile, f64)>,
}

/// A number of items and their bytes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Count {
    pub items: usize,
    pub bytes: u64,
}

impl Count {
    /// Counts one more item of `bytes`.
    pub fn add(&mut self, bytes: u64) {
        self.items += 1;
        self.bytes += bytes;
    }

    /// Counts one item of `bytes` less, stopping at zero.
    pub fn remove(&mut self, bytes: u64) {
        self.items = self.items.saturating_sub(1);
        self.bytes = self.bytes.saturating_sub(bytes);
    }
}

#[derive(Clone, Debug, Default)]
pub struct MeasureProgress {
    pub done: u64,
    pub requested: u64,
}

/// What deleting a set of nodes is expected to free: their disk usage,
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
    pub rule: RuleIx,
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
    /// `PROBLEMS_KEPT` (1000).
    pub problems: Vec<(NodeRef, String)>,
}

/// Items not deleted that a [`DeleteSummary`] lists.
pub const PROBLEMS_KEPT: usize = 1000;

impl Status {
    #[must_use]
    pub fn running_jobs(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| matches!(j.phase, JobPhase::Running | JobPhase::Finishing))
            .count()
    }

    #[must_use]
    pub fn busy(&self) -> bool {
        self.running_jobs() > 0
            || self.settling
            || self.deleting.is_some()
            || self.measuring.is_some()
    }

    pub(super) fn push_anomaly(&mut self, s: String) {
        if self.anomalies.len() < 50 {
            self.anomalies.push(s);
        }
    }

    pub(super) fn push_notice(&mut self, s: String) {
        if self.notices.len() >= 100 {
            self.notices.remove(0);
        }
        self.notices.push(s);
        self.notices_added += 1;
    }
}

#[cfg(test)]
mod tests;
