//! Delete plans: checking and queueing them, running one at a time, and applying the
//! deleter's results for their items. What an item is checked against and carries is in
//! `item`; the deleter's other messages in `deleter`.

mod deleter;
mod item;

use std::collections::HashSet;

use dfdu_clean::{DeleteResult, DeleterReq, Mode, Plan};
use dfdu_scan::Backend;
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Tree};

use super::work::Work;
use super::{Applier, DeleteProgress, DeleteSummary, Model, PROBLEMS_KEPT};

pub(super) struct ActivePlan {
    id: u64,
    mode: Mode,
    total: usize,
    remaining: HashSet<NodeRef>,
    summary: DeleteSummary,
}

impl<B: Backend> Applier<B> {
    /// Plans for the deleter thread, taken by the runtime after each slice.
    pub fn take_plans(&mut self) -> Vec<Plan> {
        std::mem::take(&mut self.outgoing_plans)
    }

    /// Undo and commit requests for the deleter thread, sent after the plans.
    pub fn take_deleter_controls(&mut self) -> Vec<DeleterReq> {
        std::mem::take(&mut self.outgoing_controls)
    }

    fn plan_nodes(&self) -> impl Iterator<Item = NodeRef> + '_ {
        let active = self
            .active_plan
            .iter()
            .flat_map(|p| p.remaining.iter().copied());
        let queued = self
            .queued_plans
            .iter()
            .flat_map(|p| p.items.iter().map(|i| i.node));
        active.chain(queued)
    }

    /// Whether `ix`, an ancestor or a descendant is in the running or a queued plan.
    pub(super) fn plan_overlaps(&self, tree: &Tree, ix: Ix) -> bool {
        self.plan_nodes()
            .filter_map(|r| tree.resolve(r))
            .any(|p| p == ix || Self::is_ancestor(tree, p, ix) || Self::is_ancestor(tree, ix, p))
    }

    /// Queues a plan for `items`. Read-only mode and imported trees are refused before this,
    /// by [`Applier::refusal`].
    pub(super) fn submit_plan(&mut self, m: &mut Model, items: Vec<NodeRef>, mode: Mode) {
        let mut out = Vec::new();
        let mut seen: HashSet<Ix> = HashSet::new();
        for r in items {
            let Some(ix) = m.tree.resolve_visible(r) else {
                continue;
            };
            if m.tree.flags(ix).has(NodeFlags::DELETING) || !seen.insert(ix) {
                continue;
            }
            if let Some(why) = self.item_refusal(&m.tree, ix) {
                m.status.notice(format!(
                    "delete: {} {why}",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            let item = item::plan_item(&m.tree, ix);
            self.cut(m, ix, DirState::Deleting);
            m.tree.set_deleting(ix, true);
            out.push(item);
        }
        if out.is_empty() {
            return;
        }
        let root = out
            .first()
            .and_then(|i| m.tree.resolve(i.node))
            .map_or_else(
                || dfdu_tree::HostPath::empty(m.tree.host()),
                |mut ix| {
                    while let Some(p) = m.tree.parent(ix) {
                        ix = p;
                    }
                    m.tree.host_path(ix)
                },
            );
        let id = self.next_plan;
        self.next_plan += 1;
        self.queued_plans.push_back(Plan {
            id,
            mode,
            scan_root: root,
            items: out,
            protected: m.rules.as_ref().map(|r| r.protected().clone()),
        });
        self.start_next_plan(m);
    }

    fn start_next_plan(&mut self, m: &mut Model) {
        if self.active_plan.is_some() {
            if let Some(d) = m.status.deleting.as_mut() {
                d.queued_plans = self.queued_plans.len();
            }
            return;
        }
        while let Some(mut plan) = self.queued_plans.pop_front() {
            // Re-check items queued behind another plan: they may have gone meanwhile.
            plan.items
                .retain(|i| m.tree.resolve_visible(i.node).is_some());
            if plan.items.is_empty() {
                continue;
            }
            let remaining: HashSet<NodeRef> = plan.items.iter().map(|i| i.node).collect();
            m.status.deleting = Some(DeleteProgress {
                plan: plan.id,
                total: remaining.len(),
                done: 0,
                queued_plans: self.queued_plans.len(),
            });
            self.active_plan = Some(ActivePlan {
                id: plan.id,
                mode: plan.mode,
                total: remaining.len(),
                remaining,
                summary: DeleteSummary {
                    plan: plan.id,
                    mode: Some(plan.mode),
                    ..Default::default()
                },
            });
            self.outgoing_plans.push(plan);
            return;
        }
        m.status.deleting = None;
    }

    pub(super) fn retry_deferred_rescans(&mut self, m: &mut Model) {
        for r in std::mem::take(&mut self.deferred_rescans) {
            match m.tree.resolve_visible(r) {
                None => {}
                Some(ix) if self.plan_overlaps(&m.tree, ix) => self.deferred_rescans.push(r),
                Some(_) => self.rescan(m, r, None, false),
            }
        }
    }

    /// Applies the deleter's result for one item of the running plan.
    fn item_result(
        &mut self,
        m: &mut Model,
        plan: u64,
        node: NodeRef,
        bytes: u64,
        result: &DeleteResult,
    ) {
        let Some(active) = self.active_plan.as_mut().filter(|a| a.id == plan) else {
            return;
        };
        active.remaining.remove(&node);
        active.summary.add(node, bytes, result);
        let mode = active.mode;
        let done = active.total - active.remaining.len();
        if let Some(d) = m.status.deleting.as_mut() {
            d.done = done;
        }
        let ix = m.tree.resolve(node);
        if matches!(result, DeleteResult::Deleted) {
            if mode.undoable() {
                self.add_undoable(plan, mode, bytes);
            }
            if mode == Mode::Stage {
                m.status.staged.add(bytes);
            }
            if let Some(ix) = ix {
                m.tree.detach(ix);
                self.work.push_back(Work::Free);
            }
        } else {
            let name = ix
                .map(|ix| String::from_utf8_lossy(m.tree.name(ix)).into_owned())
                .unwrap_or_default();
            m.status.notice(format!("delete {name}: {result}"));
            if ix.is_some_and(|ix| unmark_kept(&mut m.tree, ix, result)) {
                self.deferred_rescans.push(node);
            }
        }
        self.retry_deferred_rescans(m);
    }
}

impl DeleteSummary {
    /// Counts the result for `node`, of `bytes`.
    fn add(&mut self, node: NodeRef, bytes: u64, result: &DeleteResult) {
        match result {
            DeleteResult::Deleted => {
                self.deleted += 1;
                self.bytes += bytes;
                return;
            }
            DeleteResult::Stale(_) => self.stale += 1,
            DeleteResult::Refused(_) => self.refused += 1,
            DeleteResult::Aborted => self.aborted += 1,
            DeleteResult::Failed { .. } => self.failed += 1,
        }
        if self.problems.len() < PROBLEMS_KEPT {
            self.problems.push((node, result.to_string()));
        }
    }
}

/// Clears the deleting mark of `ix`, an item the deleter kept, and says whether it is to be
/// rescanned: a directory cut for the delete, or partly deleted, no longer has known totals.
/// It is marked cut, which keeps it shown as incomplete if that rescan is cancelled or never
/// runs.
fn unmark_kept(tree: &mut Tree, ix: Ix, result: &DeleteResult) -> bool {
    tree.set_deleting(ix, false);
    let Some(d) = tree.dir_mut(ix) else {
        return false;
    };
    let was_cut = d.cut && d.state == DirState::Deleting;
    let partial = matches!(result, DeleteResult::Failed { partial: true, .. });
    // A directory on another filesystem shows none of its contents, so a part-way delete
    // inside it leaves nothing stale.
    if !(was_cut || partial) || d.state == DirState::Skipped {
        return false;
    }
    d.cut = true;
    tree.touch(ix);
    true
}

#[cfg(test)]
mod tests;
