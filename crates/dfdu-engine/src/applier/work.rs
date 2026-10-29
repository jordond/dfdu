//! Incremental work done in bounded steps between messages: freeing, hard-link re-election
//! and merging, and the walks that cancel a job or find lost directories.

use dfdu_scan::Backend;
use dfdu_tree::{DirState, ErrKind, NodeRef};

use super::{Applier, Model};
use crate::links::{Scope, VISIBLE};

#[cfg(test)]
mod tests;

pub(super) enum Work {
    Free,
    /// Drop freed links from these inodes' lists
    /// ([`LinkTable::prune`](crate::links::LinkTable::prune)).
    Prune {
        keys: Vec<(u64, u64)>,
    },
    Reelect {
        key: (u64, u64),
        scope: Scope,
    },
    FinalPass {
        keys: Vec<(u64, u64)>,
        scope: Scope,
    },
    /// A final pass over the visible scope, with its keys taken when it starts (after any
    /// merges queued before it).
    FinalPassVisible,
    Merge {
        keys: Vec<(u64, u64)>,
        from: Scope,
    },
    CancelWalk {
        job: u32,
        stack: Vec<NodeRef>,
    },
    LostWalk {
        job: u32,
        stack: Vec<NodeRef>,
    },
    FinishJob {
        job: u32,
    },
}

/// Units of incremental work per step (nodes freed, nodes walked).
pub(super) const STEP_UNITS: usize = 4096;

/// Hard links examined per step of re-election or merging: whole inodes are taken until this
/// many of their links have been looked at, since re-electing compares the paths of all of an
/// inode's links. 128 keep a step well under the 1 ms write-lock slice on a 10M-entry tree,
/// where each link's walk up the tree misses the cache.
const LINKS_PER_STEP: usize = 128;

impl<B: Backend> Applier<B> {
    /// Runs one bounded unit of incremental work.
    pub fn work_step(&mut self, m: &mut Model) {
        let Some(w) = self.work.pop_front() else {
            if self.measure_ready() {
                self.measure_step(m);
            }
            return;
        };
        match w {
            Work::Free => self.free_step(m),
            Work::Prune { mut keys } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    n += self.links.prune(&m.tree, k).max(1);
                    self.sync_extents(m, k, false);
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::Prune { keys });
                }
            }
            Work::Reelect { key, scope } => {
                let (moved, _) = self.links.reelect(&mut m.tree, key, scope);
                self.sync_extents(m, key, moved);
            }
            Work::FinalPass { mut keys, scope } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    let (moved, links) = self.links.reelect(&mut m.tree, k, scope);
                    n += links.max(1);
                    self.sync_extents(m, k, moved);
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::FinalPass { keys, scope });
                }
            }
            Work::FinalPassVisible => {
                let tree = &m.tree;
                m.subvols.retain(|r, _| tree.resolve(*r).is_some());
                let keys = self.links.shared_keys(VISIBLE);
                self.work.push_front(Work::FinalPass {
                    keys,
                    scope: VISIBLE,
                });
            }
            Work::Merge { mut keys, from } => {
                let mut n = 0;
                while n < LINKS_PER_STEP
                    && let Some(k) = keys.pop()
                {
                    let (reelect, links) = self.links.merge_key(&mut m.tree, k, from, VISIBLE);
                    n += links.max(1);
                    self.sync_extents(m, k, true);
                    if reelect {
                        self.work.push_back(Work::Reelect {
                            key: k,
                            scope: VISIBLE,
                        });
                    }
                }
                if !keys.is_empty() {
                    self.work.push_front(Work::Merge { keys, from });
                }
            }
            Work::CancelWalk { job, stack } => self.cancel_walk_step(m, job, stack),
            Work::LostWalk { job, stack } => self.lost_walk_step(m, job, stack),
            Work::FinishJob { job } => self.finish_job(m, job),
        }
        self.refresh_status(m);
    }

    /// Frees a step's worth of detached nodes, and forgets their hard links and extents.
    fn free_step(&mut self, m: &mut Model) {
        let mut freed = Vec::new();
        let done = m.tree.free_step(STEP_UNITS, |f| freed.push(f));
        let mut prune = Vec::new();
        for f in freed {
            let key = (f.dev, f.ino);
            if f.hardlink {
                let (reelect, prune_due) = self.links.remove(key, f.node);
                if let Some(scope) = reelect {
                    self.work.push_front(Work::Reelect { key, scope });
                }
                if prune_due {
                    prune.push(key);
                }
                self.sync_extents(m, key, false);
            } else if f.measured && self.extents.counted_at(key) == Some(f.node) {
                self.extents.remove(&mut m.tree, key);
            }
        }
        if !done {
            self.work.push_back(Work::Free);
        }
        if !prune.is_empty() {
            self.work.push_front(Work::Prune { keys: prune });
        }
    }

    /// Cancels a step's worth of the directories of job `job` still pending below `stack`.
    fn cancel_walk_step(&mut self, m: &mut Model, job: u32, mut stack: Vec<NodeRef>) {
        let mut n = 0;
        while let Some(r) = stack.pop() {
            n += 1;
            let Some(ix) = m.tree.resolve(r) else {
                continue;
            };
            let Some(rec) = m.tree.dir_mut(ix) else {
                continue;
            };
            if rec.job.0 != job || rec.cut {
                continue;
            }
            if rec.state == DirState::Pending {
                rec.state = DirState::Cancelled;
                rec.close(DirState::Cancelled);
                rec.pending = 0;
                rec.reported = true;
                rec.stalled = false;
                m.tree.touch(ix);
                let kids: Vec<NodeRef> = m
                    .tree
                    .children(ix)
                    .filter(|&c| m.tree.dir(c).is_some())
                    .map(|c| m.tree.node_ref(c))
                    .collect();
                stack.extend(kids);
            }
            if n >= STEP_UNITS {
                break;
            }
        }
        if !stack.is_empty() {
            self.work.push_front(Work::CancelWalk { job, stack });
        }
    }

    /// Closes as `Lost` a step's worth of the directories of job `job` below `stack` whose
    /// listing never closed and that are not stalled.
    fn lost_walk_step(&mut self, m: &mut Model, job: u32, mut stack: Vec<NodeRef>) {
        let mut n = 0;
        while let Some(r) = stack.pop() {
            n += 1;
            let Some(ix) = m.tree.resolve(r) else {
                continue;
            };
            let Some(rec) = m.tree.dir(ix) else { continue };
            if rec.job.0 != job || rec.cut || rec.state.is_terminal() {
                continue;
            }
            let kids: Vec<NodeRef> = m
                .tree
                .children(ix)
                .filter(|&c| m.tree.dir(c).is_some())
                .map(|c| m.tree.node_ref(c))
                .collect();
            let (closed, stalled) = (rec.listing_closed, rec.stalled);
            stack.extend(kids);
            if !closed && !stalled {
                m.status.stats.lost += 1;
                m.status.incomplete = true;
                m.tree.set_error(ix, ErrKind::Other);
                self.close_listing(m, job, ix, DirState::Lost);
            }
            if n >= STEP_UNITS {
                break;
            }
        }
        if !stack.is_empty() {
            self.work.push_front(Work::LostWalk { job, stack });
        }
    }
}
