//! What a go-to prompt asks the scanned host for: the folders that complete what is typed
//! ([`tab`]), and the mounted filesystems while the path is empty or the root of the
//! filesystem ([`wants_mounts`], [`edited`]). On a scan of this machine folders are listed at
//! once; on another host's they are asked of its agent, and the mounts of either are read on a
//! thread, so the front end never waits: the session takes the answer in when it ticks
//! ([`Asking::poll`]) and the prompt picks it up for the text it was asked for
//! ([`take_answer`]). A scan read from a file lists nothing.

use crossbeam_channel::{Receiver, TryRecvError, bounded};
use dfdu_engine::wire::HostMount;
use dfdu_engine::{ListAnswer, Lister, MountsAnswer};
use dfdu_tree::Host;

use super::{Completion, Query, list_local};
use crate::state::SessionState;

/// A mounted filesystem the prompt offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Where it is mounted, as typed.
    pub point: String,
    /// Its type (`ext4`, `apfs`, `nfs4`).
    pub fstype: String,
    pub used: Option<u64>,
    pub total: Option<u64>,
}

impl Mount {
    /// Its type and how much of it is used: `xfs · 7.0 GiB of 1.0 TiB`.
    #[must_use]
    pub fn note(&self) -> String {
        match (self.used, self.total) {
            (Some(u), Some(t)) => format!(
                "{} · {} of {}",
                self.fstype,
                dfdu_engine::human(u),
                dfdu_engine::human(t)
            ),
            _ => self.fstype.clone(),
        }
    }

    /// The prompt's text once it is chosen: its mount point, ending with a separator of
    /// `host` so what is typed next goes below it.
    #[must_use]
    pub fn text(&self, host: Host) -> String {
        let mut t = self.point.clone();
        if !t.bytes().last().is_some_and(|b| host.is_sep(b)) {
            t.push(char::from(host.sep()));
        }
        t
    }

    /// The mounts of an answer that can be typed: those whose mount point is UTF-8.
    fn of(mounts: Vec<HostMount>) -> Vec<Mount> {
        (mounts.into_iter())
            .filter_map(|m| {
                Some(Mount {
                    point: String::from_utf8(m.point).ok()?,
                    fstype: m.fstype,
                    used: m.used,
                    total: m.total,
                })
            })
            .collect()
    }
}

/// What the prompt shows below the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// What a `Tab` completed.
    Completed(Completion),
    /// The host's mounts, for an empty path or the root of the filesystem.
    Mounts(Vec<Mount>),
    /// The host could not say: why.
    Failed(String),
}

/// What the scanned host was asked for and has answered, for the prompt
/// ([`SessionState::goto`]).
#[derive(Debug, Default)]
pub struct Asking {
    /// The agent's lister, on a scan of another host.
    pub remote: Option<Lister>,
    waiting: Option<Waiting>,
    answer: Option<(String, Offer)>,
}

/// An ask not answered yet, with the text it was asked for.
#[derive(Debug)]
enum Waiting {
    List {
        input: String,
        query: Query,
        rx: Receiver<ListAnswer>,
    },
    Mounts {
        input: String,
        rx: Receiver<MountsAnswer>,
    },
}

/// What a disconnected ask says: the link ended, or a thread could not be started.
const NO_ANSWER: &str = "the host did not answer";

impl Asking {
    /// Takes in an answer that arrived: true when there is one for the prompt.
    pub fn poll(&mut self) -> bool {
        let got = match &self.waiting {
            None => return false,
            Some(Waiting::List { input, query, rx }) => match rx.try_recv() {
                Ok(Ok(entries)) => {
                    let names = (entries.into_iter())
                        .filter(|e| e.dir)
                        .filter_map(|e| String::from_utf8(e.name).ok())
                        .collect();
                    (input.clone(), Offer::Completed(query.finish(names)))
                }
                Ok(Err(why)) => (input.clone(), Offer::Failed(why)),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => (input.clone(), Offer::Failed(NO_ANSWER.into())),
            },
            Some(Waiting::Mounts { input, rx }) => match rx.try_recv() {
                Ok(Ok(mounts)) => (input.clone(), Offer::Mounts(Mount::of(mounts))),
                Ok(Err(why)) => (input.clone(), Offer::Failed(why)),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => (input.clone(), Offer::Failed(NO_ANSWER.into())),
            },
        };
        self.waiting = None;
        self.answer = Some(got);
        true
    }

    /// An ask is not answered yet.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.waiting.is_some()
    }

    /// Forgets what was asked and answered: the prompt closed or its text changed.
    pub fn clear(&mut self) {
        self.waiting = None;
        self.answer = None;
    }
}

/// Whether `input` asks for the host's mounts: nothing typed, or only the root of the
/// filesystem (`/`; on a Windows host also a drive, `C:` or `C:\`).
#[must_use]
pub fn wants_mounts(input: &str, host: Host) -> bool {
    let b = input.trim().as_bytes();
    let rest = match b {
        [d, b':', rest @ ..] if host == Host::Windows && d.is_ascii_alphabetic() => rest,
        _ => b,
    };
    rest.iter().all(|&c| host.is_sep(c))
}

/// `Tab` in the prompt of the browser `s` on `input`: what it offers now, or `None` when the
/// answer comes later ([`take_answer`]). Folders of this machine are listed at once, those of
/// another host are asked of its agent; the mounts are offered for an empty path or the root.
pub fn tab(s: &mut SessionState, input: &str) -> Option<Offer> {
    let host = s.snap.dir_path.host();
    s.goto.clear();
    if wants_mounts(input, host) && ask_mounts(s, input) {
        return None;
    }
    let home = s.home.as_deref();
    let Some(query) = Query::of(input, &s.snap.dir_path, home, s.outside.local) else {
        return Some(Offer::Completed(Completion::default()));
    };
    if let Some(l) = &s.goto.remote {
        let rx = l.list(&query.dir);
        s.goto.waiting = Some(Waiting::List {
            input: input.to_owned(),
            query,
            rx,
        });
        return None;
    }
    if !s.outside.local {
        return Some(Offer::Completed(Completion::default()));
    }
    Some(Offer::Completed(match list_local(&query.dir) {
        Ok(names) => query.finish(names),
        Err(_) => Completion::default(),
    }))
}

/// The prompt's text changed to `input` (or it opened with it): what was asked for the old
/// text is forgotten, and the host's mounts are asked for when `input` wants them.
pub fn edited(s: &mut SessionState, input: &str) {
    s.goto.clear();
    if wants_mounts(input, s.snap.dir_path.host()) {
        ask_mounts(s, input);
    }
}

/// The answer for the prompt's text `input`, once: one for another text is dropped.
pub fn take_answer(s: &mut SessionState, input: &str) -> Option<Offer> {
    match s.goto.answer.take() {
        Some((asked, offer)) if asked == input => Some(offer),
        _ => None,
    }
}

/// Asks for the mounts of the host `s` scans, for the prompt's text `input`: of its agent on
/// another host's scan, read on a thread on this machine's. False when there is no host to
/// ask (a scan read from a file).
fn ask_mounts(s: &mut SessionState, input: &str) -> bool {
    let rx = match &s.goto.remote {
        Some(l) => l.mounts(),
        None if s.outside.local => local_mounts(),
        None => return false,
    };
    s.goto.waiting = Some(Waiting::Mounts {
        input: input.to_owned(),
        rx,
    });
    true
}

/// This machine's mounts, read on a thread: each filesystem gets
/// [`dfdu_sys::DISK_TIMEOUT`] to report its size, so a hung one does not hold the answer.
fn local_mounts() -> Receiver<MountsAnswer> {
    let (tx, rx) = bounded(1);
    let _ = std::thread::Builder::new()
        .name("dfdu-goto-mounts".into())
        .spawn(move || {
            let r = dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT)
                .map(|disks| {
                    (disks.into_iter())
                        .map(|d| HostMount {
                            point: dfdu_tree::name_bytes(d.mount_point.as_os_str()).into_owned(),
                            fstype: d.fstype,
                            used: d.space.map(|s| s.used),
                            total: d.space.map(|s| s.total),
                        })
                        .collect()
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
    rx
}

#[cfg(test)]
mod tests;
