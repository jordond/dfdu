//! What a stat'ed block becomes: one chunk, and a task for each subdirectory the scan reads.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{Kind, Meta};

use super::Worker;
use crate::backend::Backend;
use crate::classify::BlockParts;
use crate::proto::{
    Chunk, ChunkStatus, DirId, EntryError, FileRec, ScanMsg, SkipReason, SubdirRec,
};
use crate::shared::{Clock, JobState, Shared, Task, TaskGate, pack};

#[cfg(test)]
mod tests;

impl<B: Backend> Worker<B> {
    /// Turns the stat'ed block into chunk `seq`, creating tasks for subdirectories. Tasks go
    /// on the outbox's private stack; they become stealable when the outbox is flushed.
    /// `whole` is set when the block is the directory's whole listing.
    #[expect(clippy::expect_used, reason = "called only while a task runs")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a block holds at most `block_entries` entries, far below `u32::MAX`"
    )]
    pub(super) fn emit_block(&mut self, sh: &Shared<B>, seq: u32, whole: bool, clock: &dyn Clock) {
        let r = self.running.as_mut().expect("running");
        let block = std::mem::take(&mut self.block);
        // Epoch first: `set_focus` stores the focus before it bumps the epoch, so a new epoch
        // read here comes with the new focus. The other order could pair the old focus with
        // the new epoch and keep the old focus's children prioritised after a change.
        let epoch = sh.focus_epoch.load(SeqCst);
        let focus = sh.focus.load(SeqCst);
        let child_epoch = if focus == pack(r.task.job, r.task.dir) || r.task.focus_epoch == epoch {
            epoch
        } else {
            0
        };

        let mut files = Vec::new();
        let mut subdirs = Vec::new();
        let mut errors = Vec::new();
        let mut children = Vec::new();
        for e in block.entries {
            match e.meta {
                Err(err) => errors.push(EntryError { name: e.name, err }),
                Ok(m) if m.kind == Kind::Dir => {
                    let (mount_root, skip) =
                        subdir_skip(sh, &r.task, &r.job, &m, e.child.is_some());
                    let child = e.child.filter(|_| skip.is_none());
                    children.push(child.map(|loc| Child::new(sh, &r.task, &m, mount_root, loc)));
                    subdirs.push(SubdirRec {
                        name: e.name,
                        meta: m,
                        mount_root,
                        skip,
                    });
                }
                Ok(m) => files.push(FileRec {
                    name: e.name,
                    meta: m,
                }),
            }
        }
        let first = r.job.next_dir.fetch_add(subdirs.len() as u32, SeqCst);
        let mut class = r.class.as_ref().map(|l| {
            let others: Vec<_> = errors.iter().map(|e: &EntryError| e.name).collect();
            let (slot, loc) = (&self.slot, &r.task.loc);
            // A marker file is read under the stall watch, like any other call.
            let read = |name: &[u8], len: usize| {
                slot.watch(clock.now(), || sh.backend.read_prefix(loc, name, len))
            };
            // Following a link may wait on another mount, so it is watched too.
            let missing =
                |name: &[u8]| slot.watch(clock.now(), || sh.backend.link_target_missing(loc, name));
            let parts = BlockParts {
                names: &block.names,
                files: &files,
                subdirs: &subdirs,
                others: &others,
                first_subdir: DirId(first),
                whole,
            };
            l.block(&parts, &read, &missing)
        });
        let mut tasks = Vec::new();
        for (i, child) in children.into_iter().enumerate() {
            if let Some(c) = child {
                tasks.push(Task {
                    job: r.task.job,
                    dir: DirId(first + i as u32),
                    loc: c.loc,
                    focus_epoch: child_epoch,
                    is_root: false,
                    dev: c.dev,
                    mount: c.mount,
                    probe_subvol: c.probe_subvol,
                    chunk: None,
                    gate: c.gate,
                    storage: c.storage,
                    class: class.as_mut().and_then(|c| c.subdirs[i].take()),
                });
            }
        }
        let chunk = Chunk {
            job: r.task.job,
            dir: r.task.dir,
            seq,
            root_meta: None,
            probe: None,
            names: block.names,
            files,
            first_subdir: DirId(first),
            subdirs,
            errors,
            marks: class.map(|c| c.marks).unwrap_or_default(),
            status: ChunkStatus::Ok,
        };
        let mut ob = self.slot.outbox.lock();
        // Counted only once nothing fallible remains, so a panic above cannot leave tasks
        // counted as outstanding that will never close.
        sh.outstanding.fetch_add(tasks.len() as u64, SeqCst);
        sh.submitted_seq.fetch_add(tasks.len() as u64, SeqCst);
        ob.push(ScanMsg::Chunk(chunk), clock.now());
        // Reverse so the first subdirectory is popped first (depth-first, in read order).
        ob.tasks.extend(tasks.into_iter().rev());
    }
}

/// A subdirectory the scan will read: its task's fields, known before its id is.
struct Child<L> {
    loc: L,
    dev: u64,
    mount: u64,
    probe_subvol: bool,
    gate: Option<Arc<TaskGate<L>>>,
    storage: Storage,
}

impl<L> Child<L> {
    /// The subdirectory `m` of the task's directory, at `loc`. A mount root takes its mount's
    /// limit and storage; any other directory inherits its parent's.
    fn new<B: Backend<Loc = L>>(
        sh: &Shared<B>,
        task: &Task<L>,
        m: &Meta,
        mount_root: bool,
        loc: L,
    ) -> Self {
        let (storage, gate) = if mount_root {
            sh.gate_for(m.mount, m.dev, false)
        } else {
            (task.storage, task.gate.clone())
        };
        Child {
            loc,
            dev: m.dev,
            mount: m.mount,
            probe_subvol: m.dev != task.dev,
            gate,
            storage,
        }
    }
}

/// Whether the subdirectory `m` of the task's directory is a mount root, and why it is not
/// read, if it is not. Kernel filesystems are never read; network ones only with
/// `JobOptions::network`, other mounts only with `cross_mounts`, both decided from the mount
/// table without touching the mount; a mount the user entered is read either way. `has_loc`
/// is false for a directory the backend gave no location for, which cannot be read.
fn subdir_skip<B: Backend>(
    sh: &Shared<B>,
    task: &Task<B::Loc>,
    job: &JobState,
    m: &Meta,
    has_loc: bool,
) -> (bool, Option<SkipReason>) {
    let unreadable = (!has_loc).then_some(SkipReason::Excluded);
    // Mount ids when both are known; otherwise a change of `dev` (which also happens at btrfs
    // subvolumes, so they are only traversed with mount ids).
    let mount_root = if m.mount != 0 && task.mount != 0 {
        m.mount != task.mount
    } else {
        m.dev != task.dev
    };
    if !mount_root {
        return (false, unreadable);
    }
    let entered = job.opts.enter.contains(&(m.dev, m.ino));
    let wanted = match sh.backend.mount_class(m.mount, m.dev) {
        Some(FsClass::Kernel) => Err(SkipReason::Kernel),
        _ if entered => Ok(()),
        Some(FsClass::Network) if job.opts.network => Ok(()),
        Some(FsClass::Network) => Err(SkipReason::Network),
        _ if job.opts.cross_mounts => Ok(()),
        _ => Err(SkipReason::OtherFilesystem),
    };
    if let Err(why) = wanted {
        return (true, Some(why));
    }
    // Without a mount table this records the mount as entered, so it comes after every check
    // that can turn the mount away.
    let duplicate = match &job.dup_mounts {
        Some(d) => d.contains(&m.mount),
        None => !job.mount_roots.lock().insert((m.dev, m.ino)),
    };
    let skip = if duplicate {
        Some(SkipReason::DuplicateMount)
    } else {
        unreadable
    };
    (true, skip)
}
