//! The resume of a tree read from a file: the change journal asked what changed since, on a
//! thread of its own (it can take a second or more), then the root stat'ed there, off the
//! applier, which does no I/O (a root on a network filesystem that stopped answering stalls
//! that thread alone), and [`Command::Resume`] sent with both.
//!
//! The resume is counted as sent before the journal is asked, so that waiting for idle waits
//! for it, and uncounted on every path that does not send it: a query that panics sends it
//! without changes (everything is rescanned), and an engine that stops cancels it. A root that
//! is gone, or cannot be stat'ed, is sent as such: the resume lists it again, and its rescan
//! says why.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use dfdu_scan::journal::{JournalPosition, changed_since_cancellable};
use dfdu_scan::{Backend, JobOptions, Shared};
use dfdu_tree::{NodeRef, Tree};

use super::Engine;
use crate::applier::Command;

/// What the change journal is asked: the directories changed below a root since a position,
/// as [`changed_since_cancellable`] answers, ending early (with `None`) once the flag is set.
pub type Query = fn(&Path, JournalPosition, &AtomicBool) -> Option<Vec<PathBuf>>;

/// How long dropping an engine waits for a journal query still running.
const DROP_WAIT: Duration = Duration::from_millis(200);

/// A journal query on its thread, whose resume is counted as sent.
pub(super) struct Asking {
    thread: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    /// Nothing is sent on it: it disconnects as the thread ends, which [`Asking::stop`] waits
    /// for.
    done: Receiver<()>,
}

/// What the thread needs to ask, to stat the root and to send the resume.
struct Ask<B: Backend> {
    tx: Sender<Command>,
    sent: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    node: NodeRef,
    opts: JobOptions,
    path: PathBuf,
    at: JournalPosition,
    query: Query,
    /// What stats the root: none for a remote scan, whose root is the agent's.
    scan: Option<Arc<Shared<B>>>,
}

impl<B: Backend> Clone for Ask<B> {
    fn clone(&self) -> Self {
        Ask {
            tx: self.tx.clone(),
            sent: self.sent.clone(),
            cancel: self.cancel.clone(),
            node: self.node,
            opts: self.opts.clone(),
            path: self.path.clone(),
            at: self.at,
            query: self.query,
            scan: self.scan.clone(),
        }
    }
}

impl<B: Backend> Engine<B> {
    /// Loads `tree`, read from a file ([`Engine::load`]), and brings its root up to date with
    /// `opts` ([`Command::Resume`]): only the directories changed since `journal` when the
    /// volume's change journal can tell, otherwise all of it. A tree without a root is only
    /// loaded.
    ///
    /// The journal is asked on a thread of its own (it can take a second or more), so this
    /// returns once the tree is loaded; [`Engine::wait_idle`] waits for the resume as for any
    /// command sent.
    pub fn resume(&self, tree: Tree, journal: Option<JournalPosition>, opts: JobOptions) {
        self.resume_with(tree, journal, opts, changed_since_cancellable);
    }

    /// [`Engine::resume`], with the change journal asked by `query` (a test's, to answer late
    /// or not at all).
    pub fn resume_with(
        &self,
        tree: Tree,
        journal: Option<JournalPosition>,
        opts: JobOptions,
        query: Query,
    ) {
        let Some(root) = tree.roots().next() else {
            self.load(tree);
            return;
        };
        let node = tree.node_ref(root);
        let path = tree.path_buf(root);
        self.load(tree);
        let Some(at) = journal else {
            self.send(Command::Resume {
                node,
                opts,
                changed: None,
                since: 0,
                root: None,
            });
            return;
        };
        // Counted as sent now, so that waiting for idle waits for it.
        self.commands_sent.fetch_add(1, SeqCst);
        let ask = Ask {
            tx: self.cmd_tx.clone(),
            sent: self.commands_sent.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            node,
            opts,
            path,
            at,
            query,
            scan: self.link.is_none().then(|| self.shared.clone()),
        };
        let (inline, cancel) = (ask.clone(), ask.cancel.clone());
        let (ending, done) = bounded::<()>(0);
        match thread::Builder::new()
            .name("dfdu-journal".into())
            .spawn(move || {
                let _ending = ending;
                ask.run();
            }) {
            Ok(t) => {
                let mut asking = self.asking.lock();
                asking.retain(|a| !a.finished());
                asking.push(Asking {
                    thread: Some(t),
                    cancel,
                    done,
                });
            }
            Err(_) => inline.run(),
        }
    }
}

impl<B: Backend> Ask<B> {
    /// Asks the journal, then stats the root when it answered, and sends the resume with
    /// both, unless cancelled by then.
    fn run(self) {
        // A query (or stat) that panics cannot tell what changed: everything is rescanned.
        let (changed, root) = catch_unwind(AssertUnwindSafe(|| {
            let changed = (self.query)(&self.path, self.at, &self.cancel);
            let root = match (&changed, &self.scan) {
                (Some(_), Some(s)) if !self.cancel.load(SeqCst) => s
                    .backend
                    .stat_root(&s.backend.loc_from_path(&self.path))
                    .ok()
                    .map(Box::new),
                _ => None,
            };
            (changed, root)
        }))
        .unwrap_or((None, None));
        if self.cancel.load(SeqCst) {
            self.sent.fetch_sub(1, SeqCst);
            return;
        }
        let resume = Command::Resume {
            node: self.node,
            opts: self.opts,
            changed,
            since: self.at.time,
            root,
        };
        if self.tx.send(resume).is_err() {
            self.sent.fetch_sub(1, SeqCst);
        }
    }
}

impl Asking {
    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Tells the thread not to send the resume.
    pub(super) fn cancel(&self) {
        self.cancel.store(true, SeqCst);
    }

    /// Cancels and waits until `deadline` for the thread to end; the query sees the cancel within
    /// about 50 ms, and a thread still running then is left behind and sends nothing.
    pub(super) fn stop(&mut self, deadline: Instant) {
        self.cancel();
        let Some(t) = self.thread.take() else {
            return;
        };
        if let Err(RecvTimeoutError::Disconnected) = self.done.recv_deadline(deadline) {
            let _ = t.join();
        }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.stop(Instant::now() + DROP_WAIT);
    }
}

#[cfg(test)]
mod tests;
