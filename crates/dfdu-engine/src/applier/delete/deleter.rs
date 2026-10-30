//! The deleter's messages other than its results for the items of the running plan: the end
//! of a plan, commits and undos of staged and trashed items, native commands' output, and what
//! an earlier session left unfinished.

use dfdu_clean::DeleterMsg;
use dfdu_scan::Backend;

use crate::applier::{Applier, DeleteSummary, Model, NATIVE_LINES, Status};

impl<B: Backend> Applier<B> {
    /// Applies a result from the deleter.
    pub fn apply_delete(&mut self, m: &mut Model, msg: DeleterMsg) {
        match msg {
            DeleterMsg::Item {
                plan,
                node,
                bytes,
                result,
            } => self.item_result(m, plan, node, bytes, &result),
            DeleterMsg::Done { plan, btrfs } => {
                if btrfs {
                    m.status.notice(
                        "btrfs frees the space when the transaction commits; space of deleted \
                         snapshots is freed in the background, so free space can lag"
                            .into(),
                    );
                }
                if let Some(a) = self.active_plan.take_if(|a| a.id == plan) {
                    for r in a.remaining {
                        if let Some(ix) = m.tree.resolve(r) {
                            m.tree.set_deleting(ix, false);
                        }
                    }
                    m.status.last_delete = Some(DeleteSummary { btrfs, ..a.summary });
                }
                self.retry_deferred_rescans(m);
                self.start_next_plan(m);
            }
            DeleterMsg::Committed {
                plan,
                bytes,
                result,
            } => {
                m.status.staged.remove(bytes);
                self.forget_undoable(plan, bytes);
                if let Err(e) = result {
                    m.status.notice(format!("delete staged: {e}"));
                }
            }
            DeleterMsg::Restored {
                plan: _,
                original,
                staged,
                bytes,
                result,
            } => {
                if staged {
                    m.status.staged.remove(bytes);
                }
                match result {
                    Ok(to) => {
                        if to != original {
                            m.status.notice(format!(
                                "undo: {} exists again; restored as {}",
                                original.display(),
                                to.display()
                            ));
                        }
                        self.rescan_paths.push(to);
                    }
                    Err(e) => m.status.notice(format!("undo {}: {e}", original.display())),
                }
            }
            DeleterMsg::NativeOutput { id, line } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    if n.lines.len() == NATIVE_LINES {
                        n.lines.pop_front();
                    }
                    n.lines.push_back(line);
                }
            }
            DeleterMsg::NativeDone { id, result } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    let rescan = !n.dry_run && result.is_ok();
                    n.done = Some(result);
                    if rescan {
                        self.rescan_paths(m);
                    }
                }
                self.rescan_paths.clear();
            }
            DeleterMsg::UndoDone { plan } => {
                if m.status.undoing == Some(plan) {
                    m.status.undoing = None;
                }
                self.rescan_paths(m);
            }
            DeleterMsg::Unfinished { items, notes } => {
                notice_unfinished(&mut m.status, &items, notes);
            }
        }
        self.refresh_undo(m);
        self.refresh_status(m);
    }
}

/// Notices of the deletes an earlier session left unfinished, and of the deletion journal.
fn notice_unfinished(status: &mut Status, items: &[String], notes: Vec<String>) {
    if let Some(first) = items.first() {
        let more = match items.len() {
            1 => String::new(),
            n => format!("; and {} more", n - 1),
        };
        status.notice(format!(
            "an earlier session left {} delete{} unfinished, kept as they are \
             (dfdu recover lists them): {first}{more}",
            items.len(),
            if items.len() == 1 { "" } else { "s" },
        ));
    }
    for n in notes {
        status.notice(format!("deletion journal: {n}"));
    }
}

#[cfg(test)]
mod tests;
