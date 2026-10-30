//! A resume's check of the directories it keeps: a change journal can lose a change (`FSEvents`
//! on a loaded machine folds one made in the seconds before it is asked into an event it still
//! holds), so each loaded directory the journal does not name is stat'ed again by the scan
//! workers ([`Shared::verify`](dfdu_scan::Shared::verify)) while the walk takes the tree over,
//! and listed again like a named one when it is not as it was loaded.
//!
//! What it sees is what a directory's own stat shows: its entries changed when its mtime
//! moved (whole seconds, as the tree keeps it), and it was removed or replaced when it is gone
//! or another inode, which lists its parent again. A change within the second its scan read it
//! leaves the mtime as it was: with the time the journal position was read (`since`), which is
//! before the loaded tree's scan began, a directory whose mtime is not older is listed again
//! whatever its stat says. A file rewritten in place, which changes no directory's mtime, is
//! seen only when the journal names it.

use std::collections::HashSet;
use std::path::PathBuf;

use dfdu_scan::{Backend, JobId, Shared, VerifyDir};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Tree};

/// Directories stat'ed by one task: about a millisecond's work for a worker.
const BATCH: usize = 256;

/// The check of the directories a resume keeps, as its walk reaches them.
pub(in crate::applier) struct Verify {
    /// The root, where the batches say they are, and its device: directories on other
    /// devices are below mounts, scanned in full.
    root: PathBuf,
    dev: u64,
    /// Seconds since the epoch from which a directory's mtime cannot vouch for it; 0 for none.
    since: i64,
    /// The directories the journal names, listed again anyway.
    named: HashSet<NodeRef>,
    /// Reached and not sent yet.
    pub(super) next: Vec<NodeRef>,
    /// The directories of each batch sent, until it is answered.
    pub(super) batches: Vec<Option<Vec<NodeRef>>>,
    pub(super) waiting: usize,
}

impl Verify {
    pub(in crate::applier) fn new(tree: &Tree, root: Ix, named: &[Ix], since: u64) -> Self {
        Verify {
            root: tree.path_buf(root),
            dev: tree.dev_ino(root).0,
            since: i64::try_from(since).unwrap_or(i64::MAX),
            named: named.iter().map(|&d| tree.node_ref(d)).collect(),
            next: Vec::new(),
            batches: Vec::new(),
            waiting: 0,
        }
    }

    /// Adds directory `ix`, which the walk reached, if it is checked: not the root, nor one
    /// named, on the root's device and no mount, and read when it was scanned.
    pub(in crate::applier) fn add(&mut self, tree: &Tree, ix: Ix) {
        let r = tree.node_ref(ix);
        let checked = tree.parent(ix).is_some()
            && tree.dev_ino(ix).0 == self.dev
            && !tree.flags(ix).has(NodeFlags::MOUNT_ROOT)
            && tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped)
            && !self.named.contains(&r);
        if checked {
            self.next.push(r);
        }
    }

    /// Sends the full batches reached, and with `all` the rest, to the workers for the resume
    /// of job `job`.
    pub(in crate::applier) fn send<B: Backend>(
        &mut self,
        tree: &Tree,
        scan: &Shared<B>,
        job: u32,
        all: bool,
    ) {
        while self.next.len() >= BATCH || (all && !self.next.is_empty()) {
            let n = self.next.len().min(BATCH);
            let (mut refs, mut dirs) = (Vec::with_capacity(n), Vec::with_capacity(n));
            for r in self.next.drain(..n) {
                let Some(ix) = tree.resolve_visible(r) else {
                    continue;
                };
                let (dev, ino) = tree.dev_ino(ix);
                let mtime = tree.mtime(ix);
                refs.push(r);
                dirs.push(VerifyDir {
                    path: tree.path_buf(ix),
                    dev,
                    ino,
                    mtime: (self.since == 0 || mtime < self.since).then_some(mtime),
                });
            }
            if refs.is_empty() {
                continue;
            }
            let batch = u32::try_from(self.batches.len()).unwrap_or(u32::MAX);
            self.batches.push(Some(refs));
            self.waiting += 1;
            scan.verify(
                JobId(job),
                batch,
                scan.backend.loc_from_path(&self.root),
                dirs,
            );
        }
    }

    /// Whether answers are still to come.
    pub(in crate::applier) fn waiting(&self) -> bool {
        self.waiting > 0
    }

    /// Takes the answer to `batch`: the directories to list again, as found in `tree`. Of one
    /// not `complete`, every directory and its parent. A batch already answered gives nothing.
    pub(in crate::applier) fn answer(
        &mut self,
        tree: &Tree,
        batch: u32,
        changed: &[u32],
        gone: &[u32],
        complete: bool,
    ) -> Vec<NodeRef> {
        let Some(refs) = self.batches.get_mut(batch as usize).and_then(Option::take) else {
            return Vec::new();
        };
        self.waiting -= 1;
        // One gone or replaced is not there to list: its parent lists what is there now.
        let parent = |r: NodeRef| {
            let ix = tree.resolve_visible(r)?;
            Some(tree.node_ref(tree.parent(ix)?))
        };
        if !complete {
            let parents: Vec<NodeRef> = refs.iter().filter_map(|&r| parent(r)).collect();
            return refs.into_iter().chain(parents).collect();
        }
        let at = |i: u32| refs.get(i as usize).copied();
        changed
            .iter()
            .filter_map(|&i| at(i))
            .chain(gone.iter().filter_map(|&i| at(i).and_then(parent)))
            .collect()
    }
}
