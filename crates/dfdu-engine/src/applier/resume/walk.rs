//! A resume's walk over the loaded tree, a step at a time, and the rescans that end it.

use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, DirId, JobId};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Tree};

use super::{kept_by_target, topmost};
use crate::applier::work::{STEP_UNITS, Work};
use crate::applier::{Applier, Model};
use crate::links::{LinkTable, VISIBLE};
use crate::reclassify::ClassifyLoaded;

/// A resume's walk over the loaded tree, a step at a time: the directories still to take over,
/// and what the rescans that follow it need.
pub(in crate::applier) struct ResumeWalk {
    /// The resume's job, which holds the loaded tree.
    id: u32,
    /// The loaded root.
    root: NodeRef,
    /// The directories to list again.
    wanted: Vec<NodeRef>,
    /// Mount roots and subvolumes met so far, scanned again in full.
    mounts: Vec<NodeRef>,
    /// The classification, when the job classifies, which takes the walk's directories in turn.
    class: Option<(ClassOpts, ClassifyLoaded)>,
    /// The directories still to take over when it does not.
    stack: Vec<NodeRef>,
    /// Cancelled while it walked: its rescans are cancelled as they start.
    cancelled: bool,
}

impl ResumeWalk {
    pub(in crate::applier) fn new(
        id: u32,
        root: NodeRef,
        wanted: Vec<NodeRef>,
        class: Option<(ClassOpts, ClassifyLoaded)>,
        stack: Vec<NodeRef>,
    ) -> Self {
        ResumeWalk {
            id,
            root,
            wanted,
            mounts: Vec::new(),
            class,
            stack,
            cancelled: false,
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
            tree.node_ref(c)
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
    /// One step of a resume's walk; once it has taken the whole tree over, starts its rescans.
    pub(in crate::applier) fn resume_step(&mut self, m: &mut Model, mut walk: Box<ResumeWalk>) {
        let Some(root) = m.tree.resolve_visible(walk.root) else {
            return;
        };
        let ResumeWalk {
            id,
            mounts,
            class,
            stack,
            ..
        } = &mut *walk;
        let (id, links) = (*id, &mut self.links);
        let done = match class {
            Some((c, w)) => w.step(&mut m.tree, c, &self.scan.backend, STEP_UNITS, |t, ix| {
                own(t, links, id, root, ix, mounts);
            }),
            None => {
                let mut n = 0;
                while n < STEP_UNITS
                    && let Some(r) = stack.pop()
                {
                    let Some(ix) = m.tree.resolve_visible(r) else {
                        continue;
                    };
                    own(&mut m.tree, links, id, root, ix, mounts);
                    let t = &m.tree;
                    n += 1 + t.children(ix).count();
                    stack.extend(
                        t.children(ix)
                            .filter(|&c| t.dir(c).is_some())
                            .map(|c| t.node_ref(c)),
                    );
                }
                stack.is_empty()
            }
        };
        if done {
            self.resume_rescans(m, root, &walk);
        } else {
            self.work.push_front(Work::Resume(walk));
        }
    }

    /// Ends a resume's walk: the loaded files are measured where files can share extents, and
    /// the changed directories and the mounts below are listed again.
    fn resume_rescans(&mut self, m: &mut Model, root: Ix, walk: &ResumeWalk) {
        let tree = &m.tree;
        let mounts: Vec<Ix> = walk
            .mounts
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        let mut wanted: Vec<Ix> = walk
            .wanted
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        // Snapshots keep no measured extents: the loaded files are measured again where files
        // can share them (rescanned parts are measured when their rescans end).
        for ix in std::iter::once(root).chain(mounts.iter().copied()) {
            let dev = m.tree.dev_ino(ix).0;
            if !self.extent_devs.contains(&dev) {
                let loc = self.scan.backend.loc_from_path(&m.tree.path_buf(ix));
                if self.scan.backend.shares_extents(&loc) {
                    self.extent_devs.insert(dev);
                }
            }
        }
        self.start_measure(m, walk.root);
        // The loaded tree entered them: so do the rescans whose listings meet them.
        for &ix in &mounts {
            let at = m.tree.dev_ino(ix);
            if !self.entered.contains(&at) {
                self.entered.push(at);
            }
        }
        wanted.extend(mounts.iter().copied());
        let targets = topmost(&m.tree, wanted.clone());
        let kept = kept_by_target(&m.tree, root, &wanted, &targets, &mounts);
        m.tree.bump_version();
        self.work.push_back(Work::FinalPassVisible);
        self.queue_checks(m);
        for (t, k) in targets.into_iter().zip(kept) {
            let r = m.tree.node_ref(t);
            let first = self.next_job;
            self.rescan_keeping(m, r, None, false, k);
            if walk.cancelled {
                for id in first..self.next_job {
                    self.cancel(id);
                }
            }
        }
    }

    /// Whether a resume's walk is still going.
    pub(in crate::applier) fn resuming(&self) -> bool {
        self.work.iter().any(|w| matches!(w, Work::Resume(_)))
    }

    /// Marks the resume walking as cancelled, if `job` is its job (all when `None`).
    pub(in crate::applier) fn cancel_resume(&mut self, job: Option<u32>) {
        for w in &mut self.work {
            if let Work::Resume(walk) = w
                && job.is_none_or(|j| j == walk.id)
            {
                walk.cancelled = true;
            }
        }
    }
}
