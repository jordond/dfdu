//! The deleter thread: runs one plan at a time, item by item, journalling each.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, unbounded};
use dfdu_tree::{ErrKind, Kind};

use crate::journal::Journal;
use crate::{DeleteResult, DeleterMsg, Mode, Plan, PlanItem, Refusal, verify};

pub struct DeleterHandle {
    pub plans: Sender<Plan>,
    /// Set to stop the running plan after the current item; cleared when the next plan starts.
    pub abort: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}

/// Starts the deleter. Results go to `out`. Without a journal directory, or if the journal
/// cannot be created, every item is refused: nothing is deleted without a record.
pub fn spawn_deleter(journal_dir: Option<PathBuf>, out: Sender<DeleterMsg>) -> DeleterHandle {
    let (tx, rx): (Sender<Plan>, Receiver<Plan>) = unbounded();
    let abort = Arc::new(AtomicBool::new(false));
    let ab = abort.clone();
    let thread = thread::Builder::new()
        .name("dfdu-deleter".into())
        .spawn(move || {
            let mut journal: Option<Journal> = None;
            for plan in rx {
                ab.store(false, SeqCst);
                if journal.is_none() {
                    journal = journal_dir.as_deref().and_then(|d| Journal::create(d).ok());
                }
                let mut btrfs = false;
                for item in &plan.items {
                    let result = if ab.load(SeqCst) {
                        DeleteResult::Aborted
                    } else {
                        match journal.as_mut() {
                            None => DeleteResult::Refused(Refusal::NoJournal),
                            Some(j) => run_item(j, &plan, item),
                        }
                    };
                    btrfs |= result == DeleteResult::Deleted && on_btrfs(&item.path);
                    let _ = out.send(DeleterMsg::Item {
                        plan: plan.id,
                        node: item.node,
                        bytes: item.bytes,
                        result,
                    });
                }
                if let Some(j) = journal.as_mut() {
                    let _ = j.sync();
                }
                let _ = out.send(DeleterMsg::Done {
                    plan: plan.id,
                    btrfs,
                });
            }
        })
        .expect("spawn deleter");
    DeleterHandle {
        plans: tx,
        abort,
        thread,
    }
}

/// Whether the deleted item's directory is on btrfs.
fn on_btrfs(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    if let Some(p) = path.parent() {
        return dfdu_sys::is_btrfs(p).unwrap_or(false);
    }
    let _ = path;
    false
}

fn run_item(journal: &mut Journal, plan: &Plan, item: &PlanItem) -> DeleteResult {
    if let Err(r) = verify(item, &plan.scan_root, plan.mode) {
        let _ = journal.result(plan.id, item, &r);
        return r;
    }
    if let Err(e) = journal.intent(plan.id, plan.mode, item) {
        return DeleteResult::Failed {
            partial: false,
            err: ErrKind::from(&e),
            msg: format!("journal: {e}"),
        };
    }
    let result = execute(&item.path, item.identity.kind, plan.mode);
    let _ = journal.result(plan.id, item, &result);
    result
}

fn execute(path: &Path, kind: Kind, mode: Mode) -> DeleteResult {
    let r = match mode {
        Mode::Trash => trash::delete(path).map_err(|e| (ErrKind::Other, e.to_string())),
        // Never crosses into another mount; destroys btrfs subvolumes where allowed.
        Mode::Permanent => {
            crate::remove(path, kind).map_err(|e| (ErrKind::from(&e), e.to_string()))
        }
    };
    match r {
        Ok(()) => DeleteResult::Deleted,
        Err((err, msg)) => {
            // If the path is still there, part of it may already be gone.
            let partial = kind == Kind::Dir && fs::symlink_metadata(path).is_ok();
            DeleteResult::Failed { partial, err, msg }
        }
    }
}
