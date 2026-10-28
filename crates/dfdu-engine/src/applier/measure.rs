//! Shared-extent measurement: walking measured subtrees, requests for the measurers, their
//! results, and keeping measurements on the charged hard link.

use dfdu_scan::Backend;
use dfdu_tree::{Kind, NodeFlags, NodeRef};

use super::work::STEP_UNITS;
use super::{Applier, Model};
use crate::extents::Inode;
use crate::links::VISIBLE;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};

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

impl<B: Backend> Applier<B> {
    pub(super) fn start_measure(&mut self, m: &mut Model, root: NodeRef) {
        if !self.measure || self.extent_devs.is_empty() {
            return;
        }
        m.status.measuring.get_or_insert_with(Default::default);
        self.measure_stack.push(root);
    }

    pub(super) fn measure_ready(&self) -> bool {
        !self.measure_stack.is_empty() && self.measure_inflight < MEASURE_INFLIGHT
    }

    /// Walks part of a measured subtree, requesting each file that can share extents and is
    /// not measured yet (for hard links, the charged link).
    pub(super) fn measure_step(&mut self, m: &mut Model) {
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
                        .counted_at(inode)
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
                    .counted_at(msg.inode)
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
    pub(super) fn sync_extents(&mut self, m: &mut Model, key: Inode, moved: bool) {
        let Some(at) = self.extents.counted_at(key) else {
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
            Some(c) if c != at => self.extents.move_to(&mut m.tree, key, c),
            None if !self.links.has(key) => self.extents.remove(&mut m.tree, key),
            // Counted where it is, or a re-election is pending.
            Some(_) | None => {}
        }
    }

    /// Abandons a measurement round that has had no result for a while (a filesystem not
    /// answering), so the engine does not stay busy forever.
    pub(super) fn measure_watchdog(&mut self, m: &mut Model, now: u64) {
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
}
