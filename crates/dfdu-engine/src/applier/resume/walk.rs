//! A resume's walk over the loaded tree, a step at a time, the check of the directories it
//! keeps that runs beside it, and the start of the rescans that end both.

use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, DirId, JobId};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Tree};

use super::reclassify::ClassifyLoaded;
use super::rescans::Rescans;
use super::verify::Verify;
use crate::applier::links::{LinkTable, VISIBLE};
use crate::applier::work::{STEP_UNITS, Work};
use crate::applier::{Applier, Model};

/// A resume's walk over the loaded tree, a step at a time: the directories still to take over,
/// and what the rescans that follow it need.
pub(in crate::applier) struct ResumeWalk {
    /// The resume's job, which holds the loaded tree.
    id: u32,
    /// The loaded root.
    pub(super) root: NodeRef,
    /// The directories to list again.
    pub(super) wanted: Vec<NodeRef>,
    /// Mount roots and subvolumes met so far, scanned again in full.
    pub(super) mounts: Vec<NodeRef>,
    /// The classification, when the job classifies, which takes the walk's directories in turn.
    class: Option<(ClassOpts, ClassifyLoaded)>,
    /// The directories still to take over when it does not.
    stack: Vec<NodeRef>,
    /// Cancelled while it walked, checked or started its rescans: no more of them start.
    pub(super) cancelled: bool,
    /// It has taken the whole tree over.
    walked: bool,
    /// The check of the directories it keeps, which its rescans wait for; none for a remote
    /// scan, whose directories this host cannot stat, and once it has ended.
    pub(super) verify: Option<Verify>,
    /// Its rescans, once the walk and the check have ended.
    pub(super) rescans: Option<Rescans>,
}

impl ResumeWalk {
    pub(in crate::applier) fn new(
        id: u32,
        root: NodeRef,
        wanted: Vec<NodeRef>,
        class: Option<(ClassOpts, ClassifyLoaded)>,
        stack: Vec<NodeRef>,
        verify: Option<Verify>,
    ) -> Self {
        ResumeWalk {
            id,
            root,
            wanted,
            mounts: Vec::new(),
            class,
            stack,
            cancelled: false,
            walked: false,
            verify,
            rescans: None,
        }
    }
}

/// Takes loaded directory `ix` over for job `id`: its hard links are registered and, below
/// `root`, a mount root or subvolume is added to `mounts`.
fn own(
    tree: &mut Tree,
    links: &mut LinkTable,
    id: u32,
    root: Ix,
    ix: Ix,
    mounts: &mut Vec<NodeRef>,
) {
    if let Some(d) = tree.dir_mut(ix) {
        d.job = JobId(id);
        d.dir_id = DirId::ROOT;
    }
    // Its hard links are the visible ones, which the links rescans find are merged with; one
    // of each inode is charged, the lowest path once the rescans end.
    let linked: Vec<Ix> = tree
        .children(ix)
        .filter(|&c| tree.flags(c).has(NodeFlags::HARDLINK))
        .collect();
    for c in linked {
        links.add_scanned(tree.dev_ino(c), 0, VISIBLE, |charged| {
            tree.set_charged(c, charged);
            (tree.node_ref(c), ())
        });
    }
    // Mounts below keep journals of their own, or none, and so do btrfs subvolumes below
    // (another device, no mount), each with a tree and a generation of its own: they are
    // scanned again in full.
    let other_dev = tree
        .parent(ix)
        .is_some_and(|p| tree.dev_ino(p).0 != tree.dev_ino(ix).0);
    if ix != root
        && (tree.flags(ix).has(NodeFlags::MOUNT_ROOT) || other_dev)
        && tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped)
    {
        mounts.push(tree.node_ref(ix));
    }
}

impl<B: Backend> Applier<B> {
    /// One step of a resume's walk, which hands the directories it reaches to the check, and
    /// once it has taken the whole tree over and the check has ended, of starting its rescans.
    /// Until the check ends the walk waits aside ([`Resumes::waiting`](super::Resumes)).
    pub(in crate::applier) fn resume_step(&mut self, m: &mut Model, mut walk: Box<ResumeWalk>) {
        let Some(root) = m.tree.resolve_visible(walk.root) else {
            if walk.verify.is_some() {
                self.scanner_end_job(JobId(walk.id));
            }
            return;
        };
        if !walk.walked {
            walk.walked = self.walk_step(m, root, &mut walk);
        }
        if !walk.walked {
            self.work.push_front(Work::Resume(walk));
            return;
        }
        if walk.verify.as_ref().is_some_and(Verify::waiting) && !walk.cancelled {
            self.resume.waiting = Some(walk);
            return;
        }
        if walk.rescans.is_none() {
            if walk.verify.take().is_some() {
                self.scanner_end_job(JobId(walk.id));
            }
            walk.rescans = Some(self.resume_rescans(m, root, &walk));
        }
        if !self.rescans_step(m, &mut walk) {
            self.work.push_front(Work::Resume(walk));
        }
    }

    /// Takes a step's worth of the loaded tree over; whether it has all of it.
    fn walk_step(&mut self, m: &mut Model, root: Ix, walk: &mut ResumeWalk) -> bool {
        let ResumeWalk {
            id,
            mounts,
            class,
            stack,
            cancelled,
            verify,
            ..
        } = walk;
        let (id, links) = (*id, &mut self.links);
        // Nothing more is checked once cancelled: the rescans that would use it are not run.
        let mut verify = verify.as_mut().filter(|_| !*cancelled);
        let done = if let Some((c, w)) = class {
            w.step(&mut m.tree, c, &self.scan.backend, STEP_UNITS, |t, ix| {
                own(t, links, id, root, ix, mounts);
                if let Some(v) = verify.as_deref_mut() {
                    v.add(t, ix);
                }
            })
        } else {
            let mut n = 0;
            while n < STEP_UNITS
                && let Some(r) = stack.pop()
            {
                let Some(ix) = m.tree.resolve_visible(r) else {
                    continue;
                };
                own(&mut m.tree, links, id, root, ix, mounts);
                let t = &m.tree;
                if let Some(v) = verify.as_deref_mut() {
                    v.add(t, ix);
                }
                n += 1 + t.children(ix).count();
                stack.extend(
                    t.children(ix)
                        .filter(|&c| t.dir(c).is_some())
                        .map(|c| t.node_ref(c)),
                );
            }
            stack.is_empty()
        };
        if let Some(v) = verify {
            v.send(&m.tree, &self.scan, id, done);
        }
        done
    }

    /// Applies what a batch of the check of resume `job` found: what changed is listed again
    /// by its rescans. The last answer lets a walk that waits for it go on.
    pub(in crate::applier) fn resume_verified(
        &mut self,
        m: &Model,
        job: u32,
        batch: u32,
        (changed, gone): (&[u32], &[u32]),
        complete: bool,
    ) {
        let Some(walk) = self.resume_walk(job) else {
            return;
        };
        let Some(v) = walk.verify.as_mut() else {
            return;
        };
        let again = v.answer(&m.tree, batch, changed, gone, complete);
        let waiting = v.waiting();
        walk.wanted.extend(again);
        if !waiting && let Some(w) = self.resume.waiting.take_if(|w| w.id == job) {
            self.work.push_front(Work::Resume(w));
        }
    }

    /// Whether resume `job` is walking or waiting, with a check.
    pub(in crate::applier) fn resume_checks(&self, job: u32) -> bool {
        let walking = self.work.iter().find_map(|w| match w {
            Work::Resume(walk) if walk.id == job => Some(&**walk),
            _ => None,
        });
        let waiting = self.resume.waiting.as_deref().filter(|w| w.id == job);
        waiting.or(walking).is_some_and(|w| w.verify.is_some())
    }

    /// The walk of resume `job`, walking or waiting for its check.
    fn resume_walk(&mut self, job: u32) -> Option<&mut ResumeWalk> {
        let waiting = self.resume.waiting.as_deref_mut().filter(|w| w.id == job);
        waiting.or_else(|| {
            self.work.iter_mut().find_map(|w| match w {
                Work::Resume(walk) if walk.id == job => Some(&mut **walk),
                _ => None,
            })
        })
    }

    /// Whether a resume's walk, its check or the start of its rescans is still going.
    pub(in crate::applier) fn resuming(&self) -> bool {
        self.resume.waiting.is_some() || self.work.iter().any(|w| matches!(w, Work::Resume(_)))
    }

    /// Marks the resume walking, checking or starting its rescans as cancelled, if `job` is
    /// its job (all when `None`): its check ends at once, its walk goes on, and no more of its
    /// rescans start.
    pub(in crate::applier) fn cancel_resume(&mut self, job: Option<u32>) {
        let walking = self.work.iter_mut().filter_map(|w| match w {
            Work::Resume(walk) => Some(&mut **walk),
            _ => None,
        });
        let mut checking = Vec::new();
        for walk in walking.chain(self.resume.waiting.as_deref_mut()) {
            if job.is_none_or(|j| j == walk.id) {
                walk.cancelled = true;
                if walk.verify.is_some() {
                    checking.push(walk.id);
                }
            }
        }
        for id in checking {
            self.scanner_end_job(JobId(id));
        }
        if let Some(w) = self.resume.waiting.take_if(|w| w.cancelled) {
            self.work.push_front(Work::Resume(w));
        }
    }
}

#[cfg(test)]
mod tests;
