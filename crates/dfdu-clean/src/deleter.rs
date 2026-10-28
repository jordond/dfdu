//! The deleter thread: runs one plan at a time, item by item, journalling each; deletes staged
//! items when their undo window ends; puts items back on undo.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender, unbounded};
use dfdu_tree::{ErrKind, Kind};

use crate::journal::Journal;
use crate::recover;
use crate::stage::Stager;
use crate::{
    DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal,
    rename_noreplace, verify,
};

pub struct DeleterHandle {
    pub reqs: Sender<DeleterReq>,
    /// Set to stop the running plan after the current item; cleared when the next plan starts.
    pub abort: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}

/// How long a staged item can be put back before it is deleted in the background.
pub const STAGE_UNDO_WINDOW: Duration = Duration::from_secs(60);

/// An item the session can still put back.
struct Kept {
    plan: u64,
    item: PlanItem,
    /// When it went to the trash, in seconds since the epoch.
    at: i64,
    /// Where it is staged; `None` for the trash.
    staged: Option<PathBuf>,
    /// When a staged item is committed.
    due: Instant,
}

struct State {
    journal_dir: Option<PathBuf>,
    journal: Option<Journal>,
    /// Why the journal could not be created, when it could not.
    journal_err: Option<(ErrKind, String)>,
    stager: Option<Stager>,
    out: Sender<DeleterMsg>,
    kept: Vec<Kept>,
    window: Duration,
}

/// Starts the deleter. Results go to `out`. Without a journal directory, or if the journal
/// cannot be created, every item is refused: nothing is deleted without a record. Staged items
/// are deleted `window` after they were staged ([`STAGE_UNDO_WINDOW`] in the app), or on
/// [`DeleterReq::Commit`]; those still staged when the deleter stops are left for recovery
/// (`crate::recover`).
///
/// # Panics
///
/// When the deleter's thread cannot be started.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "nothing can be deleted without the thread; `thread::spawn` panics the same way"
)]
pub fn spawn_deleter(
    journal_dir: Option<PathBuf>,
    window: Duration,
    out: Sender<DeleterMsg>,
) -> DeleterHandle {
    let (tx, rx) = unbounded::<DeleterReq>();
    let abort = Arc::new(AtomicBool::new(false));
    let ab = abort.clone();
    let thread = thread::Builder::new()
        .name("dfdu-deleter".into())
        .spawn(move || {
            let mut st = State {
                journal_dir,
                journal: None,
                journal_err: None,
                stager: None,
                out,
                kept: Vec::new(),
                window,
            };
            if let Some(d) = st.journal_dir.as_deref() {
                let (items, notes) = match recover::scan(d) {
                    Ok(r) => (
                        r.items.iter().map(recover::Unfinished::describe).collect(),
                        r.notes,
                    ),
                    Err(e) => (vec![], vec![format!("{}: {e}", d.display())]),
                };
                if !(items.is_empty() && notes.is_empty()) {
                    let _ = st.out.send(DeleterMsg::Unfinished { items, notes });
                }
            }
            loop {
                let next = st
                    .kept
                    .iter()
                    .filter(|k| k.staged.is_some())
                    .map(|k| k.due)
                    .min();
                let req = match next {
                    Some(t) => match rx.recv_deadline(t) {
                        Ok(r) => Some(r),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(r) => Some(r),
                        Err(_) => break,
                    },
                };
                match req {
                    None => st.commit(Some(Instant::now())),
                    Some(DeleterReq::Plan(plan)) => {
                        ab.store(false, SeqCst);
                        st.run_plan(&plan, &ab);
                    }
                    Some(DeleterReq::Undo { plan }) => st.undo(plan),
                    Some(DeleterReq::Native(req)) => {
                        ab.store(false, SeqCst);
                        st.run_native(&req, &ab);
                    }
                    Some(DeleterReq::Commit) => st.commit(None),
                }
            }
            if let Some(j) = st.journal.as_mut() {
                let _ = j.sync();
            }
        })
        .expect("spawn deleter");
    DeleterHandle {
        reqs: tx,
        abort,
        thread,
    }
}

impl State {
    fn open_journal(&mut self) {
        if self.journal.is_some() {
            return;
        }
        let Some(dir) = self.journal_dir.as_deref() else {
            return;
        };
        match Journal::create(dir) {
            Ok(j) => {
                let base = dir.parent().map(|p| p.join("stage"));
                self.stager = Some(Stager::new(j.session(), base));
                self.journal = Some(j);
                self.journal_err = None;
            }
            Err(e) => {
                self.journal_err = Some((
                    ErrKind::from(&e),
                    format!("the deletion journal could not be written: {e}"),
                ));
            }
        }
    }

    /// What an item gets when there is no journal: refused without a journal directory, failed
    /// with the cause when the journal could not be created.
    fn no_journal(&self) -> DeleteResult {
        match &self.journal_err {
            Some((err, msg)) => DeleteResult::Failed {
                partial: false,
                err: *err,
                msg: msg.clone(),
            },
            None => DeleteResult::Refused(Refusal::NoJournal),
        }
    }

    fn run_plan(&mut self, plan: &Plan, ab: &AtomicBool) {
        self.open_journal();
        let mut btrfs = false;
        for item in &plan.items {
            let result = if ab.load(SeqCst) {
                DeleteResult::Aborted
            } else {
                match (self.journal.as_mut(), self.stager.as_mut()) {
                    (Some(j), Some(s)) => {
                        let at = dfdu_tree::now_secs();
                        let (r, staged) = run_item(j, s, plan, item);
                        if r == DeleteResult::Deleted && plan.mode.undoable() {
                            self.kept.push(Kept {
                                plan: plan.id,
                                item: item.clone(),
                                at,
                                staged,
                                due: Instant::now() + self.window,
                            });
                        }
                        r
                    }
                    _ => self.no_journal(),
                }
            };
            btrfs |= result == DeleteResult::Deleted && on_btrfs(&item.path);
            let _ = self.out.send(DeleterMsg::Item {
                plan: plan.id,
                node: item.node,
                bytes: item.bytes,
                result,
            });
        }
        if let Some(j) = self.journal.as_mut() {
            let _ = j.sync();
        }
        let _ = self.out.send(DeleterMsg::Done {
            plan: plan.id,
            btrfs,
        });
    }

    /// Runs a native command. The real one is journalled like a delete: nothing runs without
    /// its record.
    fn run_native(&mut self, req: &crate::NativeReq, ab: &AtomicBool) {
        let result = if req.dry_run {
            crate::native::run(req, ab, &self.out)
        } else {
            self.open_journal();
            match self.journal.as_mut() {
                None => Err(self.no_journal().to_string()),
                Some(j) => {
                    if let Err(e) = j.native(req) {
                        Err(format!("journal: {e}"))
                    } else {
                        let r = crate::native::run(req, ab, &self.out);
                        let _ = j.native_done(req.id, &r);
                        r
                    }
                }
            }
        };
        let _ = self.out.send(DeleterMsg::NativeDone { id: req.id, result });
    }

    /// Deletes the staged items due by `by`, or all of them.
    fn commit(&mut self, by: Option<Instant>) {
        let (due, keep): (Vec<Kept>, Vec<Kept>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|k| k.staged.is_some() && by.is_none_or(|t| k.due <= t));
        self.kept = keep;
        let Some(j) = self.journal.as_mut() else {
            return;
        };
        for k in due {
            // Only staged items are due.
            let Some(staged) = k.staged.as_deref() else {
                continue;
            };
            let result = j
                .commit_intent(k.plan, &k.item)
                .map_err(|e| format!("journal: {e}"))
                .and_then(|()| {
                    fault::commit_hang(staged);
                    let id = &k.item.identity;
                    crate::remove_verified(staged, id.kind, id.dev, id.ino).map_err(|e| {
                        format!(
                            "{} stays staged at {}: {e}",
                            k.item.path.display(),
                            staged.display()
                        )
                    })
                })
                .and_then(|()| {
                    j.commit_done(k.plan, &k.item)
                        .map_err(|e| format!("journal: {e}"))
                });
            let _ = self.out.send(DeleterMsg::Committed {
                plan: k.plan,
                bytes: k.item.bytes,
                result,
            });
        }
    }

    fn undo(&mut self, plan: u64) {
        let (mine, keep): (Vec<Kept>, Vec<Kept>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|k| k.plan == plan);
        self.kept = keep;
        for k in mine {
            let result = match &k.staged {
                Some(s) => crate::stage::put_back(s, &k.item.path),
                None => crate::stage::restore_from_trash(&k.item.path, &k.item.identity, k.at),
            }
            .map_err(|e| e.to_string());
            // The item is back either way; a lost record only makes a later recovery list it
            // as still staged, so the user is told rather than the undo reported as failed.
            if let (Ok(to), Some(j)) = (&result, self.journal.as_mut())
                && let Err(e) = j.restored(plan, &k.item, to)
            {
                let _ = self.out.send(DeleterMsg::Unfinished {
                    items: vec![],
                    notes: vec![format!(
                        "{} was restored to {}, but the journal could not record it: {e}",
                        k.item.path.display(),
                        to.display()
                    )],
                });
            }
            let _ = self.out.send(DeleterMsg::Restored {
                plan,
                original: k.item.path.clone(),
                staged: k.staged.is_some(),
                bytes: k.item.bytes,
                result,
            });
        }
        let _ = self.out.send(DeleterMsg::UndoDone { plan });
    }
}

/// Whether the deleted item's directory is on btrfs.
#[cfg(target_os = "linux")]
fn on_btrfs(path: &Path) -> bool {
    path.parent()
        .is_some_and(|p| dfdu_sys::is_btrfs(p).unwrap_or(false))
}

#[cfg(not(target_os = "linux"))]
fn on_btrfs(_path: &Path) -> bool {
    false
}

fn run_item(
    journal: &mut Journal,
    stager: &mut Stager,
    plan: &Plan,
    item: &PlanItem,
) -> (DeleteResult, Option<PathBuf>) {
    let checked = verify(item, &plan.scan_root, plan.mode).and_then(|()| {
        if item.identity.kind == Kind::Dir {
            crate::inside::compare(&item.path, &item.inside).map_err(DeleteResult::Stale)
        } else {
            Ok(())
        }
    });
    if let Err(r) = checked {
        let _ = journal.result(plan.id, item, &r);
        return (r, None);
    }
    let stage = if plan.mode == Mode::Stage {
        match stager.target(item, &plan.scan_root) {
            Ok(t) => Some(t),
            Err(e) => {
                let r = DeleteResult::Failed {
                    partial: false,
                    err: ErrKind::from(&e),
                    msg: e.to_string(),
                };
                let _ = journal.result(plan.id, item, &r);
                return (r, None);
            }
        }
    } else {
        None
    };
    if let Err(e) = journal.intent(plan.id, plan.mode, item, stage.as_deref()) {
        return (
            DeleteResult::Failed {
                partial: false,
                err: ErrKind::from(&e),
                msg: format!("journal: {e}"),
            },
            None,
        );
    }
    let result = execute(&item.path, &item.identity, plan.mode, stage.as_deref());
    let _ = journal.result(plan.id, item, &result);
    (result, stage)
}

fn execute(path: &Path, id: &Identity, mode: Mode, stage: Option<&Path>) -> DeleteResult {
    let kind = id.kind;
    let r = match (mode, stage) {
        (Mode::Trash, _) => trash::delete(path).map_err(|e| (ErrKind::Other, e.to_string())),
        // A rename: all or nothing.
        (Mode::Stage, Some(to)) => {
            rename_noreplace(path, to).map_err(|e| (ErrKind::from(&e), e.to_string()))
        }
        (Mode::Stage, None) => Err((ErrKind::Other, "no staging path".into())),
        // Never crosses into another mount; destroys btrfs subvolumes where allowed.
        (Mode::Permanent, _) => crate::remove_verified(path, kind, id.dev, id.ino)
            .map_err(|e| (ErrKind::from(&e), e.to_string())),
    };
    match r {
        Ok(()) => DeleteResult::Deleted,
        Err((err, msg)) => {
            // If the path is still there, part of it may already be gone.
            let partial =
                mode == Mode::Permanent && kind == Kind::Dir && fs::symlink_metadata(path).is_ok();
            DeleteResult::Failed { partial, err, msg }
        }
    }
}

/// Test hooks (the `fault-injection` feature).
mod fault {
    use std::path::Path;

    /// With `DFDU_FAULT_COMMIT_HANG` set, a staged commit deletes one entry of the item and
    /// then never returns, so a test can kill the process part-way through.
    #[cfg(feature = "fault-injection")]
    pub fn commit_hang(staged: &Path) {
        if std::env::var_os("DFDU_FAULT_COMMIT_HANG").is_none() {
            return;
        }
        if let Some(e) = std::fs::read_dir(staged)
            .ok()
            .and_then(|mut d| d.next()?.ok())
        {
            let _ = std::fs::remove_file(e.path());
        }
        loop {
            std::thread::park();
        }
    }

    #[cfg(not(feature = "fault-injection"))]
    pub fn commit_hang(_staged: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::Receiver;

    fn plan_of(path: PathBuf, scan_root: PathBuf) -> Plan {
        let mut tree = dfdu_tree::Tree::new();
        let meta = dfdu_tree::Meta {
            kind: Kind::File,
            apparent: 1,
            alloc: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            nlink: 1,
            mount: 0,
        };
        let root = tree.new_root(b"/x", &meta, false);
        Plan {
            id: 1,
            mode: Mode::Permanent,
            scan_root,
            items: vec![PlanItem {
                node: tree.node_ref(root),
                path,
                identity: Identity {
                    dev: 0,
                    ino: 0,
                    kind: Kind::File,
                    apparent: 1,
                    mtime: 0,
                },
                bytes: 1,
                inside: std::sync::Arc::default(),
            }],
        }
    }

    /// The messages up to the plan's `Done`: its item results and any journal notes.
    fn run(
        rx: &Receiver<DeleterMsg>,
        h: &DeleterHandle,
        plan: Plan,
    ) -> (Vec<DeleteResult>, Vec<String>) {
        h.reqs.send(DeleterReq::Plan(plan)).unwrap();
        let (mut results, mut notes) = (vec![], vec![]);
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                DeleterMsg::Item { result, .. } => results.push(result),
                DeleterMsg::Unfinished { notes: n, .. } => notes.extend(n),
                DeleterMsg::Done { .. } => return (results, notes),
                _ => {}
            }
        }
    }

    #[test]
    fn without_a_journal_nothing_is_deleted_and_the_cause_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let victim = root.join("victim");
        fs::write(&victim, b"x").unwrap();

        // No journal directory at all: refused.
        let (tx, rx) = unbounded();
        let h = spawn_deleter(None, Duration::from_secs(60), tx);
        let (results, notes) = run(&rx, &h, plan_of(victim.clone(), root.clone()));
        assert_eq!(results, [DeleteResult::Refused(Refusal::NoJournal)]);
        assert!(notes.is_empty(), "{notes:?}");
        drop(h);

        // A journal directory that cannot be read or created (a file is in its place): the
        // startup scan's error is a note, and the item fails with why.
        let blocked = root.join("ops");
        fs::write(&blocked, b"").unwrap();
        let (tx, rx) = unbounded();
        let h = spawn_deleter(Some(blocked.clone()), Duration::from_secs(60), tx);
        let (results, notes) = run(&rx, &h, plan_of(victim.clone(), root.clone()));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with(&blocked.display().to_string()),
            "{notes:?}"
        );
        let [
            DeleteResult::Failed {
                partial: false,
                msg,
                ..
            },
        ] = &results[..]
        else {
            panic!("{results:?}");
        };
        assert!(
            msg.starts_with("the deletion journal could not be written: "),
            "{msg}"
        );
        assert!(victim.exists());
    }
}
