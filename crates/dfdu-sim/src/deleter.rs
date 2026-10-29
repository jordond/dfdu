//! The simulated deleter: delete plans run item by item, with results chosen at random or by
//! a scenario.

use std::collections::VecDeque;
use std::path::PathBuf;

use dfdu_clean::{DeleterMsg, Plan, Refusal};
use dfdu_engine::DeleteResult;
use dfdu_scan::{Backend, Shared};
use dfdu_tree::{ErrKind, Kind, NodeFlags, Tree};

use crate::{Rng, Sim, SimBackend, SimReport, Spec};

impl Sim {
    /// Paths of the items of the delete plan the deleter is running, if any.
    #[must_use]
    pub fn running_plan(&self) -> Option<Vec<PathBuf>> {
        self.plans
            .front()
            .map(|(p, _)| p.items.iter().map(|i| i.path.clone()).collect())
    }

    /// The deleter's result for the next item of the running plan, applied at once (a
    /// `Deleted` item is removed from the spec first). When no item is left, the plan's `Done`.
    /// Returns false if no plan is running.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn delete_next(&mut self, result: DeleteResult) -> Result<bool, String> {
        let Some((plan, i)) = self.plans.front_mut() else {
            return Ok(false);
        };
        let msg = if *i < plan.items.len() {
            let item = plan.items[*i].clone();
            *i += 1;
            if result == DeleteResult::Deleted {
                let n = self.shared.backend.loc_from_path(&item.path);
                self.spec.delete_subtree(n);
                self.report.deleted_items += 1;
            }
            DeleterMsg::Item {
                plan: plan.id,
                node: item.node,
                bytes: item.bytes,
                result,
            }
        } else {
            let id = plan.id;
            self.plans.pop_front();
            DeleterMsg::Done {
                plan: id,
                btrfs: false,
            }
        };
        self.applier.apply_delete(&mut self.model, msg);
        self.applier.refresh_status(&mut self.model);
        self.plans
            .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
        self.check(" after a delete result")?;
        Ok(true)
    }
}

/// One to three visible entries whose removal cannot shift a fault position.
pub(crate) fn pick_deletable(
    tree: &Tree,
    spec: &Spec,
    shared: &Shared<SimBackend>,
    rng: &mut Rng,
) -> Vec<dfdu_tree::NodeRef> {
    let Some(root) = tree.roots().next() else {
        return vec![];
    };
    let mut all = vec![];
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        stack.extend(tree.children(ix));
        if ix != root
            && !tree.flags(ix).has(NodeFlags::DELETING)
            && spec.deletable(shared.backend.loc_from_path(&tree.path_buf(ix)))
        {
            all.push(ix);
        }
    }
    let mut out = vec![];
    for _ in 0..=rng.below(3) {
        if all.is_empty() {
            break;
        }
        let i = rng.index(all.len());
        out.push(tree.node_ref(all.swap_remove(i)));
    }
    out
}

/// The simulated deleter: handles one item of the oldest plan, choosing a result and applying
/// it to the spec, or reports the plan done.
pub(crate) fn deleter_step(
    spec: &Spec,
    shared: &Shared<SimBackend>,
    plans: &mut VecDeque<(Plan, usize)>,
    rng: &mut Rng,
    report: &mut SimReport,
) -> Option<DeleterMsg> {
    let (plan, i) = plans.front_mut()?;
    if *i >= plan.items.len() {
        let id = plan.id;
        plans.pop_front();
        return Some(DeleterMsg::Done {
            plan: id,
            btrfs: false,
        });
    }
    let item = plan.items[*i].clone();
    *i += 1;
    let n = shared.backend.loc_from_path(&item.path);
    let is_dir = spec.nodes[n as usize].meta.kind == Kind::Dir;
    let result = match rng.below(100) {
        0..70 => {
            spec.delete_subtree(n);
            report.deleted_items += 1;
            DeleteResult::Deleted
        }
        70..78 => DeleteResult::Stale("changed".into()),
        78..82 => DeleteResult::Refused(Refusal::Protected),
        82..86 => DeleteResult::Aborted,
        86..92 => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
        _ if is_dir => {
            // Part of the tree went before the error.
            let mut stack: Vec<u32> = spec.live_children(n).collect();
            while let Some(d) = stack.pop() {
                stack.extend(spec.live_children(d));
                if spec.deletable(d) && rng.chance(500) {
                    spec.delete_subtree(d);
                }
            }
            DeleteResult::Failed {
                partial: true,
                err: ErrKind::Other,
                msg: "eio part-way".into(),
            }
        }
        _ => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
    };
    Some(DeleterMsg::Item {
        plan: plan.id,
        node: item.node,
        bytes: item.bytes,
        result,
    })
}
