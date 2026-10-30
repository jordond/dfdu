//! Applying scanner messages: the gate, chunks, closes, and pending accounting up to each
//! job's root.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use dfdu_scan::{
    Backend, Chunk, ChunkStatus, DirId, Entry, JobId, ListingStatus, Mark, Probe, ScanMsg,
    SkipReason,
};
use dfdu_tree::{Class, DirRec, DirState, ErrKind, Ix, Kind, Meta, NodeFlags, Totals, Tree};

use super::work::Work;
use super::{Applier, Job, JobPhase, Model, Slot, empty_dir_meta};
use crate::recent::Event;

#[cfg(test)]
mod tests;

impl<B: Backend> Applier<B> {
    /// Applies one scanner message.
    pub fn apply(&mut self, m: &mut Model, msg: ScanMsg, now: u64) {
        m.status.stats.messages += 1;
        m.recent.push(Event::of_msg(&msg));
        match msg {
            ScanMsg::Chunk(c) => self.apply_chunk(m, &c),
            ScanMsg::Close {
                job,
                dir,
                chunks,
                status,
                classes,
            } => self.apply_close(m, job, dir, chunks, (status, classes)),
            // A batch of a resume's check that stalls is not waited for: what it was to stat
            // is listed again, where a stall is handled as in any scan.
            ScanMsg::Stalled { job, dir } if self.resume_checks(job.0) => {
                m.status.stats.stalled += 1;
                self.resume_verified(m, job.0, dir.0, (&[], &[]), false);
                self.scanner_abandon(job, dir);
                self.refresh_status(m);
            }
            ScanMsg::Verified {
                job,
                batch,
                changed,
                gone,
                complete,
            } => {
                self.resume_verified(m, job.0, batch, (&changed, &gone), complete);
                self.refresh_status(m);
            }
            ScanMsg::Stalled { job, dir } => {
                m.status.stats.stalled += 1;
                match self.gate(m, job, dir, None) {
                    Some(ix) => {
                        let rec = dir_rec(&mut m.tree, ix);
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
            stop.extend(subdir_ids(first, n));
        }
        let j = gated(&mut self.jobs, job);
        for &d in &stop[1..] {
            j.put(d, Slot::Dead);
        }
        self.scanner_stop_dirs(job, stop);
        None
    }

    fn apply_chunk(&mut self, m: &mut Model, c: &Chunk) {
        m.status.stats.chunks += 1;
        let Some(ix) = self.gate(m, c.job, c.dir, Some((c.first_subdir, c.subdirs.len()))) else {
            return;
        };
        let rec = dir_rec(&mut m.tree, ix);
        if rec.listing_closed {
            // From a part that returned after the directory was abandoned. Its subdirectories
            // were never added, but tasks for them exist: stop them, and drop what they send
            // (as under a cut).
            m.status.stats.late += 1;
            let ids: Vec<DirId> = subdir_ids(c.first_subdir, c.subdirs.len()).collect();
            let j = gated(&mut self.jobs, c.job);
            for &d in &ids {
                j.put(d, Slot::Dead);
            }
            self.scanner_stop_dirs(c.job, ids);
            return;
        }
        if !rec.mark_seq(c.seq) {
            m.status.stats.duplicate += 1;
            return;
        }
        if let Some(meta) = &c.root_meta {
            m.tree.update_meta(ix, meta);
        }
        if let Some(p) = c.probe.as_deref() {
            self.apply_probe(m, ix, p);
        }
        m.tree
            .reserve_children(ix, c.files.len() + c.errors.len() + c.subdirs.len());
        let scope = gated(&mut self.jobs, c.job).scope;
        let mut delta = Totals::default();
        // Nodes of the entries that have marks, by entry, only when some have.
        let mut file_ix = Vec::new();
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
        let j = gated(&mut self.jobs, c.job);
        let (new_pending, subdir_ix) = add_subdirs(&mut m.tree, ix, c, j, marked, &mut delta);
        apply_marks(m, c, &file_ix, &subdir_ix);
        m.tree.propagate_add(ix, &delta);
        m.status.stats.entries += (c.files.len() + c.errors.len() + c.subdirs.len()) as u64;
        let rec = dir_rec(&mut m.tree, ix);
        rec.pending += new_pending;
        rec.chunks_seen += 1;
        let seen = rec.chunks_seen;
        if c.status != ChunkStatus::Ok {
            m.tree.touch(ix);
        }
        let j = gated(&mut self.jobs, c.job);
        if j.held_close.get(&c.dir.0).is_some_and(|(n, _)| *n == seen)
            && let Some((_, status)) = j.held_close.remove(&c.dir.0)
        {
            self.close_with_status(m, c.job.0, ix, status);
        }
    }

    /// What a chunk's probe says of its directory: whether it shares extents, its btrfs data
    /// profile and subvolume.
    fn apply_probe(&mut self, m: &mut Model, ix: Ix, p: &Probe) {
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
        let rec = dir_rec(&mut m.tree, ix);
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
        let j = gated(&mut self.jobs, job);
        if j.held_close.contains_key(&dir.0) {
            m.status.stats.duplicate += 1;
            return;
        }
        match seen.cmp(&chunks) {
            Ordering::Equal => self.close_with_status(m, job.0, ix, status),
            Ordering::Greater => {
                m.status.stats.corrupt += 1;
                m.status.incomplete = true;
                m.status.anomaly(format!(
                    "close with {chunks} chunks after {seen}: job {} dir {}",
                    job.0, dir.0
                ));
                self.close_with_status(m, job.0, ix, status);
            }
            Ordering::Less => {
                j.held_close.insert(dir.0, (chunks, status));
            }
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
                if let Some(cx) = j.live(d).and_then(|r| m.tree.resolve(r)) {
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
    pub(super) fn close_listing(&mut self, m: &mut Model, job: u32, ix: Ix, outcome: DirState) {
        let rec = dir_rec(&mut m.tree, ix);
        debug_assert!(!rec.listing_closed);
        rec.close(outcome);
        let dir = rec.dir_id.0;
        if rec.stalled {
            rec.stalled = false;
            if let Some(j) = self.jobs.get_mut(&job) {
                j.stalled.remove(&dir);
            }
            self.refresh_status(m);
        }
        self.measure_listing(m, job, ix);
        self.dec_pending(m, job, ix);
        self.retry_rescans_after_listing(m);
    }

    /// Decrements `ix`'s pending count and cascades completion upward.
    fn dec_pending(&mut self, m: &mut Model, job: u32, ix: Ix) {
        let rec = dir_rec(&mut m.tree, ix);
        debug_assert!(rec.pending > 0, "pending underflow");
        rec.pending = rec.pending.saturating_sub(1);
        if rec.pending > 0 {
            return;
        }
        rec.state = rec.outcome;
        m.tree.touch(ix);
        let rec = dir_rec(&mut m.tree, ix);
        if rec.reported {
            return;
        }
        rec.reported = true;
        self.report_up(m, job, ix);
    }

    pub(super) fn report_up(&mut self, m: &mut Model, job: u32, ix: Ix) {
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
        if prec.job.0 != job || prec.cut || prec.reported {
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
}

/// The ids of a chunk's `n` subdirectories, the first `first`.
fn subdir_ids(first: DirId, n: usize) -> impl Iterator<Item = DirId> {
    (first.0..).take(n).map(DirId)
}

/// Adds the chunk's subdirectories below `ix`, recording them in job `j`; the number added
/// that are to be scanned (not skipped), and their nodes by entry if the chunk has marks.
fn add_subdirs(
    tree: &mut Tree,
    ix: Ix,
    c: &Chunk,
    j: &mut Job,
    marked: bool,
    delta: &mut Totals,
) -> (u32, Vec<Ix>) {
    let mut new_pending = 0;
    let mut subdir_ix = Vec::new();
    for (s, id) in c
        .subdirs
        .iter()
        .zip(subdir_ids(c.first_subdir, c.subdirs.len()))
    {
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
        let cx = tree.add_dir_flagged(ix, c.name(s.name), &s.meta, flags);
        if marked {
            subdir_ix.push(cx);
        }
        j.set_live(id, tree.node_ref(cx));
        let crec = dir_rec(tree, cx);
        crec.job = c.job;
        crec.dir_id = id;
        if s.skip.is_some() {
            crec.state = DirState::Skipped;
            crec.close(DirState::Skipped);
            crec.pending = 0;
            crec.reported = true;
            // It stands for the loaded directory until the rescan swaps in.
            if s.skip == Some(SkipReason::Kept)
                && let Some(k) = j.kept.as_mut()
            {
                k.placeholders.push(tree.node_ref(cx));
            }
        } else {
            new_pending += 1;
        }
        delta.add(&tree.own(cx));
    }
    (new_pending, subdir_ix)
}

/// Applies the chunk's marks to the nodes of its entries; a mark naming no entry is corrupt.
fn apply_marks(m: &mut Model, c: &Chunk, file_ix: &[Ix], subdir_ix: &[Ix]) {
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
}

/// The record of directory `ix`.
///
/// # Panics
///
/// If `ix` is not a directory.
#[expect(
    clippy::expect_used,
    reason = "job slots, gated messages and pending counts only ever name directories"
)]
fn dir_rec(tree: &mut Tree, ix: Ix) -> &mut DirRec {
    tree.dir_mut(ix).expect("a directory")
}

/// Job `job`, which [`Applier::gate`] found running.
///
/// # Panics
///
/// If there is no such job.
#[expect(
    clippy::expect_used,
    reason = "called for a job the gate found, which nothing removes while its message applies"
)]
fn gated(jobs: &mut BTreeMap<u32, Job>, job: JobId) -> &mut Job {
    jobs.get_mut(&job.0).expect("a gated job")
}
