//! Shared-extent measurement: walking measured subtrees, requests for the measurers, their
//! results, and keeping measurements on the charged hard link.

use std::collections::HashMap;

use dfdu_scan::Backend;
use dfdu_tree::{Ix, Kind, NodeFlags, NodeRef, Tree};

use super::links::VISIBLE;
use super::work::STEP_UNITS;
use super::{Applier, JobKind, JobPhase, Model};
use crate::extents::Inode;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};

#[cfg(test)]
mod tests;

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

/// Where a measurement walk goes next.
#[derive(Clone, Copy)]
enum MeasureAt {
    /// A directory whose listing closed: its own files.
    Files(NodeRef),
    /// A subtree: the files of each directory in it not queued already.
    Subtree(NodeRef),
}

/// What the applier keeps of extent measurement: where its walk goes next and the requests
/// it has in flight.
#[derive(Default)]
pub(super) struct Measuring {
    stack: Vec<MeasureAt>,
    /// The node each inode with a measurement in flight was requested for.
    awaited: HashMap<Inode, NodeRef>,
    /// Requests for the measurers, taken by the runtime after each slice.
    outgoing: Vec<MeasureReq>,
    /// Files in the requests in flight.
    inflight: usize,
    /// Results of earlier rounds (abandoned after a timeout) are ignored.
    round: u64,
    /// When the last result came, or when nothing was in flight.
    last: u64,
}

impl<B: Backend> Applier<B> {
    /// Queues what is left to measure below `root`, whose job ended.
    pub(super) fn start_measure(&mut self, m: &mut Model, root: NodeRef) {
        if !self.settings.measure || self.extent_devs.is_empty() {
            return;
        }
        m.status.measuring.get_or_insert_with(Default::default);
        self.measure.stack.push(MeasureAt::Subtree(root));
    }

    /// Queues the files of directory `ix`, whose listing just closed, when its filesystem can
    /// share extents: they are measured while the scan goes on. Only for initial scans, whose
    /// tree is visible; a rescan's shadow is measured when it swaps in.
    pub(super) fn measure_listing(&mut self, m: &mut Model, job: u32, ix: Ix) {
        if !self.settings.measure
            || self.extent_devs.is_empty()
            || !self.extent_devs.contains(&m.tree.dev_ino(ix).0)
        {
            return;
        }
        let Some(j) = self
            .jobs
            .get_mut(&job)
            .filter(|j| j.kind == JobKind::Initial)
        else {
            return;
        };
        j.measures = true;
        if let Some(rec) = m.tree.dir_mut(ix) {
            rec.measure_queued = true;
        }
        m.status.measuring.get_or_insert_with(Default::default);
        self.measure
            .stack
            .push(MeasureAt::Files(m.tree.node_ref(ix)));
    }

    pub(super) fn measure_ready(&self) -> bool {
        !self.measure.stack.is_empty() && self.measure.inflight < MEASURE_INFLIGHT
    }

    /// Nothing is queued or in flight, and no running scan queues more: the progress shown
    /// ends.
    pub(super) fn measure_idle(&self) -> bool {
        self.measure.stack.is_empty()
            && self.measure.inflight == 0
            && !self
                .jobs
                .values()
                .any(|j| j.measures && matches!(j.phase, JobPhase::Running | JobPhase::Finishing))
    }

    /// Whether a measurement of `inode` is in flight for a node still visible.
    fn awaited(&self, tree: &Tree, inode: Inode) -> bool {
        self.measure
            .awaited
            .get(&inode)
            .is_some_and(|&r| tree.resolve_visible(r).is_some())
    }

    /// Walks part of what is queued, requesting each file that can share extents and is not
    /// measured or in flight yet (for hard links, the charged link).
    pub(super) fn measure_step(&mut self, m: &mut Model) {
        let mut units = 0;
        while units < STEP_UNITS && self.measure.inflight < MEASURE_INFLIGHT {
            let Some(at) = self.measure.stack.pop() else {
                break;
            };
            units += 1;
            let (r, below) = match at {
                MeasureAt::Files(r) => (r, false),
                MeasureAt::Subtree(r) => (r, true),
            };
            let Some(ix) = m.tree.resolve_visible(r) else {
                continue;
            };
            // A directory queued when its listing closed has its files requested by that entry.
            let own = match m.tree.dir_mut(ix) {
                Some(rec) if below => !std::mem::replace(&mut rec.measure_queued, true),
                _ => true,
            };
            let tree = &m.tree;
            let mut files: Vec<MeasureFile> = Vec::new();
            for c in tree.children(ix) {
                units += 1;
                if tree.dir(c).is_some() {
                    if below {
                        self.measure
                            .stack
                            .push(MeasureAt::Subtree(tree.node_ref(c)));
                    }
                    continue;
                }
                if !own {
                    continue;
                }
                let f = tree.flags(c);
                let inode = tree.dev_ino(c);
                if tree.kind(c) != Kind::File
                    || f.has(NodeFlags::MEASURED)
                    || tree.raw_size(c).1 < self.settings.measure_min.max(1)
                    || !self.extent_devs.contains(&inode.0)
                    || (f.has(NodeFlags::HARDLINK) && !f.has(NodeFlags::CHARGED))
                    || self
                        .extents
                        .counted_at(inode)
                        .is_some_and(|p| tree.resolve_visible(p).is_some())
                    || self.awaited(tree, inode)
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
                self.measure.inflight += files.len();
                for f in &files {
                    self.measure.awaited.insert(f.inode, f.node);
                }
                m.status
                    .measuring
                    .get_or_insert_with(Default::default)
                    .requested += files.len() as u64;
                let dir = tree.host_path(ix);
                for part in files.chunks(MEASURE_BATCH) {
                    self.measure.outgoing.push(MeasureReq {
                        round: self.measure.round,
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
        self.measure.outgoing.push(MeasureReq {
            round: self.measure.round,
            dir: tree.host_path(parent),
            files: vec![MeasureFile {
                node,
                name: tree.name(ix).to_vec(),
                inode,
            }],
        });
        self.measure.inflight += 1;
        self.measure.awaited.insert(inode, node);
        m.status
            .measuring
            .get_or_insert_with(Default::default)
            .requested += 1;
    }

    /// Requests for the measurers, taken by the runtime after each slice.
    pub fn take_measure_requests(&mut self) -> Vec<MeasureReq> {
        std::mem::take(&mut self.measure.outgoing)
    }

    pub fn apply_measure(&mut self, m: &mut Model, msg: MeasureMsg, now: u64) {
        if msg.round != self.measure.round {
            return;
        }
        self.measure.inflight = self.measure.inflight.saturating_sub(1);
        self.measure.last = now;
        if let Some(p) = m.status.measuring.as_mut() {
            p.done += 1;
        }
        if self.measure.awaited.get(&msg.inode) == Some(&msg.node) {
            self.measure.awaited.remove(&msg.inode);
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
            MeasureResult::Measured(_) | MeasureResult::Changed => {
                // A hard link whose charge moved to another link meanwhile (which waited for
                // this answer), or that is gone: measure that one, or the inode would stay
                // unmeasured.
                let tree = &m.tree;
                let measured = self
                    .extents
                    .counted_at(msg.inode)
                    .is_some_and(|p| tree.resolve_visible(p).is_some());
                if let Some(c) = self.links.charged(msg.inode, VISIBLE)
                    && c != msg.node
                    && !measured
                    && !self.awaited(tree, msg.inode)
                {
                    self.request_one(m, c, msg.inode);
                }
            }
            MeasureResult::Failed(_) => m.status.unmeasured += 1,
        }
        // Only the part of `refresh_status` a result can change: all of it per result costs a
        // tenth of the applier's time while measuring.
        if self.measure_idle() {
            m.status.measuring = None;
        }
    }

    /// Keeps an inode's measurement on its charged link as hard-link charges move, and drops
    /// it when no link remains. An inode not measured yet whose charge `moved` to a link that
    /// can be measured is requested: the walk skipped that link while it was uncharged, and a
    /// result for the old link is ignored.
    pub(super) fn sync_extents(&mut self, m: &mut Model, key: Inode, moved: bool) {
        let Some(at) = self.extents.counted_at(key) else {
            if moved
                && self.settings.measure
                && self.extent_devs.contains(&key.0)
                && !self.awaited(&m.tree, key)
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
            Some(c) if c != at => self.extents.move_to(&mut m.tree, key, c),
            None if !self.links.has(key) => self.extents.remove(&mut m.tree, key),
            // Counted where it is, or a re-election is pending.
            Some(_) | None => {}
        }
    }

    /// Abandons a measurement round that has had no result for a while (a filesystem not
    /// answering), so the engine does not stay busy forever.
    pub(super) fn measure_watchdog(&mut self, m: &mut Model, now: u64) {
        if self.measure.inflight == 0 {
            self.measure.last = now;
            return;
        }
        if now.saturating_sub(self.measure.last) < MEASURE_TIMEOUT_NS {
            return;
        }
        m.status.unmeasured += self.measure.inflight as u64;
        m.status.push_notice(
            "extent measurement stopped: a filesystem did not answer; shared data may be \
             counted more than once"
                .into(),
        );
        self.measure.round += 1;
        self.measure.inflight = 0;
        self.measure.stack.clear();
        self.measure.awaited.clear();
        self.measure.last = now;
    }
}
