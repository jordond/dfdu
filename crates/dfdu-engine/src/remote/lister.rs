//! Listing on the agent's host from the local side ([`Lister`]): a directory's entries and the
//! mounted filesystems, each asked with an id and answered through the reader
//! ([`super::pump`]). Nothing waits on the link: each ask returns a channel the answer arrives
//! on, which disconnects without one when the link ends.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

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
    /// returned; it disconnects without one when the link ends first.
    #[must_use]
    pub fn list(&self, dir: &HostPath) -> Receiver<ListAnswer> {
        let (tx, rx) = bounded(1);
        if dir.as_bytes().len() > MAX_PATH {
            let _ = tx.send(Err("the path is too long".into()));
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

    /// Sends the request `req` builds with a new id, `waiting` for its answer; on a dead link
    /// `waiting` is dropped, which disconnects its channel.
    fn ask(&self, waiting: Waiting, req: impl FnOnce(u64) -> ToAgent) {
        let link = &self.link;
        if link.dead.load(SeqCst) {
            return;
        }
        let id = link.ask_id.fetch_add(1, SeqCst);
        link.asked.lock().insert(id, waiting);
        link.send(&req(id));
        if link.dead.load(SeqCst) {
            link.asked.lock().clear();
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
