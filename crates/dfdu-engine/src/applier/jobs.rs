//! Scan job lifecycle: starting, cutting, cancelling, abandoning stalled directories, and
//! finishing.

use std::collections::HashMap;

use dfdu_scan::{Backend, DirId, JobId, JobOptions};
use dfdu_tree::{DirState, ErrKind, HostPath, Ix, NodeRef, Pages, Tree};

use super::links::Scope;
use super::work::Work;
use super::{Applier, Job, JobKind, JobPhase, Kept, Model};

#[cfg(test)]
mod tests;

/// What a job scans: the node it is for (`target`), the directory it scans into (`root`: the
/// target, or a shadow that replaces it when the job finishes), the scope of the hard links it
/// finds, and the path and options it scans with.
pub(super) struct JobStart {
    pub kind: JobKind,
    pub target: Ix,
    pub root: Ix,
    pub scope: Scope,
    pub path: HostPath,
    pub opts: JobOptions,
    pub kept: Option<Box<Kept>>,
}

impl<B: Backend> Applier<B> {
    /// Starts job `id`.
    ///
    /// # Panics
    ///
    /// If `start.root` is not a directory.
    #[expect(
        clippy::expect_used,
        reason = "callers pass a root they have just created or resolved as a directory"
    )]
    pub(super) fn start(&mut self, m: &mut Model, id: u32, start: JobStart) {
        let JobStart {
            kind,
            target,
            root,
            scope,
            path,
            opts,
            kept,
        } = start;
        let rec = m.tree.dir_mut(root).expect("job root is a directory");
        rec.job = JobId(id);
        rec.dir_id = DirId::ROOT;
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
            measures: false,
            kept,
        };
        job.set_live(DirId::ROOT, m.tree.node_ref(root));
        job.submitted_seq = self.scanner_start_job(JobId(id), &path, opts);
        self.jobs.insert(id, job);
    }

    /// Cuts a directory its job is still scanning: its job's tasks for it are stopped, and it
    /// reports to its parent at once with `state`. Does nothing to a directory that is not
    /// pending or already cut.
    pub(super) fn cut(&mut self, m: &mut Model, ix: Ix, state: DirState) {
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
            self.report_up(m, job.0, ix);
        }
        m.tree.touch(ix);
        self.scanner_stop_dirs(job, [dir]);
    }

    pub(super) fn cancel(&mut self, id: u32) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        if j.phase != JobPhase::Running {
            return;
        }
        j.phase = JobPhase::Cancelled;
        let (job, root) = (j.id, j.root);
        self.scanner_end_job(job);
        self.work.push_back(Work::CancelWalk {
            job: id,
            stack: vec![root],
        });
        self.work.push_back(Work::FinishJob { job: id });
    }

    /// Ends a remote scan whose link ended, once everything that arrived before is applied:
    /// running jobs stop where they are, which leaves the scan incomplete.
    pub(crate) fn check_link(&mut self, m: &mut Model) {
        let Some(why) = self.settings.remote.as_ref().and_then(|l| l.take_lost()) else {
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

    pub(super) fn abandon(&mut self, m: &mut Model, node: NodeRef) {
        let Some(ix) = m.tree.resolve(node) else {
            return;
        };
        let Some(rec) = m.tree.dir(ix) else { return };
        let (job, dir) = (rec.job, rec.dir_id);
        if rec.listing_closed || rec.cut {
            return;
        }
        if let Some(j) = self.jobs.get_mut(&job.0) {
            j.stalled.remove(&dir.0);
        }
        m.tree.set_error(ix, ErrKind::Stalled);
        self.close_listing(m, job.0, ix, DirState::Abandoned);
        self.scanner_abandon(job, dir);
    }

    /// Cancels job `job`, or every job, and a resume's walk with it.
    pub(super) fn cancel_jobs(&mut self, job: Option<u32>) {
        self.cancel_resume(job);
        let ids: Vec<u32> = match job {
            Some(id) => vec![id],
            None => self.jobs.keys().copied().collect(),
        };
        for id in ids {
            self.cancel(id);
        }
    }

    /// Whether `b` is below `a`.
    pub(super) fn is_ancestor(tree: &Tree, a: Ix, mut b: Ix) -> bool {
        while let Some(p) = tree.parent(b) {
            if p == a {
                return true;
            }
            b = p;
        }
        false
    }

    pub(super) fn under_cut(tree: &Tree, ix: Ix) -> bool {
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
        let mut retry = std::mem::take(&mut self.retry_abandon);
        retry.retain(|&(j, d)| !self.scanner_try_abandon(j, d));
        self.retry_abandon = retry;
        // Stalled directories that were cut or removed since they stalled are not wanted.
        let mut unwanted = Vec::new();
        for j in self.jobs.values().filter(|j| j.phase == JobPhase::Running) {
            for &d in j.stalled.keys() {
                let gone = j
                    .live(DirId(d))
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
                        && let Some(r) = j.live(DirId(d))
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

    pub(super) fn finish_job(&mut self, m: &mut Model, id: u32) {
        let Some(j) = self.jobs.get_mut(&id) else {
            return;
        };
        // A rescan that keeps loaded directories finds them first, a step at a time.
        if j.phase == JobPhase::Finishing
            && let Some(k) = j.kept.as_mut()
            && let Some(t) = m.tree.resolve_visible(j.target)
            && !Self::find_kept(m, k, t, j.opts.class.as_ref())
        {
            self.work.push_front(Work::FinishJob { job: id });
            return;
        }
        let cancelled = j.phase == JobPhase::Cancelled;
        let (job, kind, root, target, scope) = (j.id, j.kind, j.root, j.target, j.scope);
        j.phase = if cancelled {
            JobPhase::Cancelled
        } else {
            JobPhase::Done
        };
        j.map = Pages::default();
        j.held_close.clear();
        let kept = j.kept.take();
        let class = kept.as_ref().and_then(|_| j.opts.class.clone());
        let stalled: Vec<DirId> = j.stalled.drain().map(|(d, _)| DirId(d)).collect();
        self.scanner_end_job(job);
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
                    let grafted = kept.map(|mut k| Self::graft_kept(m, &mut k, t, class));
                    m.tree.replace(t, s);
                    self.start_measure(m, root);
                    let keys = self.links.keys_in(scope);
                    self.work.push_back(Work::Free);
                    self.work.push_back(Work::Merge { keys, from: scope });
                    if let Some(g) = grafted {
                        self.resume.rehome |= g.rehome;
                        self.work.push_back(Work::Grafted(Box::new(g)));
                    }
                }
                (_, Some(s), t) => {
                    m.tree.detach(s);
                    self.work.push_back(Work::Free);
                    // A resume's rescan cancelled: what it was to list again stays as loaded.
                    if cancelled && kept.is_some() && t.is_some() {
                        self.unverified_below(m, target);
                    }
                }
                _ => {}
            }
            self.rehome_when_swapped();
        }
        // Every job's end (done or cancelled) settles the visible charges to the lowest path, so
        // per-directory totals never depend on scan order.
        self.work.push_back(Work::FinalPassVisible);
        self.queue_checks(m);
        // Its listings that never closed are not waited for any more.
        self.retry_rescans_after_listing(m);
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
}
