//! Listing on the agent's host from the local side ([`Lister`]): a directory's entries and the
//! mounted filesystems, each asked with an id and answered through the reader
//! ([`super::pump`]). Nothing waits on the link: each ask is sent from a thread of its own and
//! returns a channel the answer arrives on, which disconnects without one when the link ends
//! or the thread cannot be started.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};
use dfdu_tree::{Host, HostPath};

use super::Link;
use super::wire::listing::MAX_PATH;
use super::wire::{HostMount, ListEntry, ToAgent};

/// A listing's answer: the entries, or why there are none.
pub type ListAnswer = Result<Vec<ListEntry>, String>;
/// A mount list's answer: the mounts, or why there are none.
pub type MountsAnswer = Result<Vec<HostMount>, String>;

/// Who waits for the answer to an ask, by its id.
pub(crate) enum Waiting {
    List(Sender<ListAnswer>),
    Mounts(Sender<MountsAnswer>),
}

/// Asks the agent of a remote engine what its host has outside the scan
/// ([`Engine::lister`](crate::Engine::lister)). Cheap to clone; it keeps the link's state
/// alive, not the agent.
#[derive(Clone)]
pub struct Lister {
    link: Arc<Link>,
}

impl std::fmt::Debug for Lister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lister")
            .field("host", &self.link.host)
            .finish_non_exhaustive()
    }
}

impl Lister {
    pub(crate) fn new(link: Arc<Link>) -> Self {
        Lister { link }
    }

    /// The kind of host the agent runs on.
    #[must_use]
    pub fn host(&self) -> Host {
        self.link.host
    }

    /// Asks for the entries of `dir` on the agent's host. The answer arrives on the channel
    /// returned; it disconnects without one when the link ends first. A path that is empty
    /// or too long is answered at once, without asking.
    #[must_use]
    pub fn list(&self, dir: &HostPath) -> Receiver<ListAnswer> {
        let (tx, rx) = bounded(1);
        let why = match dir.as_bytes().len() {
            0 => Some("no folder to list"),
            n if n > MAX_PATH => Some("the path is too long"),
            _ => None,
        };
        if let Some(why) = why {
            let _ = tx.send(Err(why.into()));
            return rx;
        }
        let path = dir.as_bytes().to_vec();
        self.ask(Waiting::List(tx), |id| ToAgent::List { id, path });
        rx
    }

    /// Asks for the filesystems mounted on the agent's host, as [`Lister::list`] does.
    #[must_use]
    pub fn mounts(&self) -> Receiver<MountsAnswer> {
        let (tx, rx) = bounded(1);
        self.ask(Waiting::Mounts(tx), |id| ToAgent::Mounts { id });
        rx
    }

    /// Sends the request `req` builds with a new id from a thread of its own, so the caller
    /// never waits on the link, `waiting` for its answer. On a dead link, or when the thread
    /// cannot be started, `waiting` is dropped, which disconnects its channel.
    fn ask(&self, waiting: Waiting, req: impl FnOnce(u64) -> ToAgent) {
        let link = &self.link;
        if link.dead.load(SeqCst) {
            return;
        }
        let id = link.ask_id.fetch_add(1, SeqCst);
        link.asked.lock().insert(id, waiting);
        let (l, req) = (link.clone(), req(id));
        let sent = thread::Builder::new()
            .name("dfdu-link-ask".into())
            .spawn(move || {
                l.send(&req);
                // The reader may have ended the link, and cleared what was asked, before
                // this ask was in.
                if l.dead.load(SeqCst) {
                    l.asked.lock().remove(&id);
                }
            });
        if sent.is_err() {
            link.asked.lock().remove(&id);
        }
    }
}

impl Link {
    /// Passes the answer to ask `id` to whoever waits for it; one nobody asked, or of
    /// another kind, is dropped.
    pub(super) fn answer_listed(&self, id: u64, result: ListAnswer) {
        if let Some(Waiting::List(tx)) = self.asked.lock().remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// As [`Link::answer_listed`], for a mount list.
    pub(super) fn answer_mounts(&self, id: u64, result: MountsAnswer) {
        if let Some(Waiting::Mounts(tx)) = self.asked.lock().remove(&id) {
            let _ = tx.send(result);
        }
    }
}
