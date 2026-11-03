//! Scans of another computer, chosen on the start screen ([`Target`]): ssh to it on a thread of
//! its own ([`Connecting`]), the agent there started as `dfdu --remote` starts it
//! (`dfdu_session::remote`), sending one when it has none once the user agrees. ssh's questions
//! (a password, a key's passphrase, whether to trust a new host) come through the askpass
//! helper ([`crate::askpass`]) and are asked in a dialog over the screen that says what is
//! happening; passwords are kept as [`crate::secrets`] says. Cancel stops ssh.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use dfdu_session::remote::{self, AgentMode, Cancel, Front};
use zeroize::Zeroizing;

use crate::askpass::{self, Ask, Askpass, Kind};
use crate::launch::Settings;
use crate::secrets::{self, Secrets};

mod screen;
mod target;

pub use target::{Target, decode, encode, remember};

/// What the thread connecting asks the window.
enum Event {
    Note(String),
    /// Whether to send the agent (`what`) into `dir` there.
    Send {
        what: String,
        dir: String,
        reply: Sender<bool>,
    },
}

/// The dialog over the screen.
enum Question {
    /// A password, a passphrase, or another answer ssh wants typed.
    Secret {
        ask: Ask,
        kind: Kind,
        id: Option<String>,
        input: Zeroizing<String>,
        remember: bool,
        /// The one given before was not accepted.
        again: bool,
    },
    /// Whether to trust a host whose key is not known, or another yes or no question: ssh's
    /// question as it put it.
    YesNo { ask: Ask, host_key: bool },
    Send {
        what: String,
        dir: String,
        reply: Sender<bool>,
    },
}

/// A question waiting for the system's store: what it asks, the secret's id, and what the
/// store answers.
type Lookup = (Ask, String, Receiver<Option<Zeroizing<String>>>);

/// A secret given in this connection.
struct Answered {
    secret: Zeroizing<String>,
    /// To keep in the system's store once the connection opens.
    remember: bool,
    /// The ssh it was given to.
    from: u32,
}

/// What came of connecting.
pub enum Outcome {
    Opened(Box<remote::Opened>),
    /// Why not; empty when it was cancelled.
    Failed(String),
}

/// A connection being made to `target`, until it opens or fails.
pub struct Connecting {
    pub target: Target,
    /// Listens for ssh's questions until the connection is made.
    askpass: Option<Askpass>,
    cancel: Cancel,
    done: Receiver<Result<remote::Opened, String>>,
    /// What came of it was taken: nothing is left to cancel.
    finished: bool,
    events: Receiver<Event>,
    /// The last thing it said it does.
    status: String,
    question: Option<Question>,
    /// A secret being read from the system's store for a question.
    lookup: Option<Lookup>,
    /// The secrets given in this connection, kept once it opens.
    answered: HashMap<String, Answered>,
    /// The secrets the same ssh asked for again: one given was wrong, so none of them is
    /// remembered between runs, even when a later one was right.
    rejected: HashSet<String>,
}

impl Connecting {
    /// Starts connecting to `target`, scanning with `settings`.
    pub fn start(target: Target, settings: &Settings, ctx: &egui::Context) -> Connecting {
        let cancel = Cancel::default();
        let (done_tx, done) = crossbeam_channel::bounded(1);
        let (ev_tx, events) = crossbeam_channel::unbounded();
        let askpass = Askpass::start(ctx);
        let env = match (&askpass, helper()) {
            (Ok(a), Ok(exe)) => Ok(a.env(exe)),
            (Err(e), _) => Err(e.to_string()),
            (_, Err(e)) => Err(e.to_string()),
        };
        let rules = settings.rules.then(|| {
            let (files, errors) = dfdu_rules::sources(dfdu_sys::dirs::rules_dir().as_deref());
            for e in errors {
                eprintln!("dfdu-gui: {e}");
            }
            dfdu_engine::RemoteRules { files }
        });
        let opts = settings.engine.clone();
        let (t, c, repaint) = (target.clone(), cancel.clone(), ctx.clone());
        let run = move || {
            let opened = env.and_then(|env| {
                let mut front = Window {
                    events: ev_tx,
                    ctx: repaint.clone(),
                };
                connect(&t, env, rules, opts, &mut front, &c)
            });
            let _ = done_tx.send(opened);
            repaint.request_repaint();
        };
        if let Err(e) = std::thread::Builder::new()
            .name("dfdu-gui-connect".into())
            .spawn(run)
        {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let _ = tx.send(Err(format!("cannot start a thread: {e}")));
            return Connecting::with(target, None, cancel, rx, events);
        }
        Connecting::with(target, askpass.ok(), cancel, done, events)
    }

    fn with(
        target: Target,
        askpass: Option<Askpass>,
        cancel: Cancel,
        done: Receiver<Result<remote::Opened, String>>,
        events: Receiver<Event>,
    ) -> Connecting {
        let status = format!("Connecting to {}…", target.host);
        Connecting {
            target,
            askpass,
            cancel,
            done,
            finished: false,
            events,
            status,
            question: None,
            lookup: None,
            answered: HashMap::new(),
            rejected: HashSet::new(),
        }
    }

    /// Stops connecting: ssh is killed and its questions cancelled.
    pub fn cancel(&mut self) {
        if !self.finished {
            self.cancel.cancel();
        }
        self.question = None;
        self.lookup = None;
    }

    /// Takes in what the thread and ssh said; what came of it once it is over. The secrets
    /// given are kept in `secrets` once it opens.
    pub fn poll(&mut self, secrets: &mut Secrets, ctx: &egui::Context) -> Option<Outcome> {
        if self.finished {
            return None;
        }
        if self.cancel.is_cancelled() {
            // Nothing more is asked: what comes is declined (dropped), so the thread waiting for
            // an answer goes on to its end.
            while self.events.try_recv().is_ok() {}
            while self.askpass.as_ref().and_then(Askpass::next).is_some() {}
        }
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Note(msg) => self.status = msg,
                Event::Send { what, dir, reply } => {
                    self.question = Some(Question::Send { what, dir, reply });
                }
            }
        }
        self.take_lookup();
        if self.question.is_none()
            && self.lookup.is_none()
            && let Some(ask) = self.askpass.as_ref().and_then(Askpass::next)
        {
            self.asked(ask, secrets, ctx);
        }
        let result = match self.done.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err("the connection stopped unexpectedly".into())
            }
        };
        self.finished = true;
        self.askpass = None;
        self.question = None;
        self.lookup = None;
        Some(match result {
            // Cancelled as it opened: the scan is dropped, which ends its link.
            Ok(_) | Err(_) if self.cancel.is_cancelled() => Outcome::Failed(String::new()),
            Ok(opened) => {
                for (id, a) in self.answered.drain() {
                    let remember = a.remember && !self.rejected.contains(&id);
                    secrets.keep(&id, a.secret, remember);
                }
                Outcome::Opened(Box::new(opened))
            }
            Err(e) => Outcome::Failed(e),
        })
    }

    /// Answers `ask` from what this connection, this run or the system's store has, or asks the
    /// user. The same ssh asking again for one it was given means it was wrong: it is
    /// forgotten and the user asked. Another ssh of the connection (a jump host's) gets it too.
    fn asked(&mut self, ask: Ask, secrets: &mut Secrets, ctx: &egui::Context) {
        let kind = whole_key(askpass::kind(&ask.prompt), self.target.identity.as_deref());
        if ask.confirm || kind == Kind::HostKey {
            let host_key = kind == Kind::HostKey;
            self.question = Some(Question::YesNo { ask, host_key });
            return;
        }
        let id = secrets::id(&kind, self.target.port);
        let mut again = false;
        if let Some(id) = &id {
            match self.answered.get_mut(id) {
                Some(a) if a.from == ask.from => {
                    again = true;
                    self.answered.remove(id);
                    self.rejected.insert(id.clone());
                    secrets.forget(id);
                }
                Some(a) => {
                    a.from = ask.from;
                    ask.answer(Some(a.secret.clone()));
                    return;
                }
                None => {}
            }
        }
        if !again && let Some(id) = &id {
            if let Some(s) = secrets.get(id) {
                self.give(id.clone(), s.clone(), false, ask.from);
                ask.answer(Some(s));
                return;
            }
            if secrets::store_name().is_some() {
                self.lookup = Some((ask, id.clone(), secrets.lookup(id.clone(), ctx)));
                return;
            }
        }
        self.question = Some(Question::Secret {
            ask,
            kind,
            id,
            input: Zeroizing::default(),
            remember: false,
            again,
        });
    }

    fn give(&mut self, id: String, secret: Zeroizing<String>, remember: bool, from: u32) {
        let a = Answered {
            secret,
            remember,
            from,
        };
        self.answered.insert(id, a);
    }

    /// The secret read from the system's store, when it came: answered with, or asked for.
    fn take_lookup(&mut self) {
        let Some((_, _, rx)) = &self.lookup else {
            return;
        };
        let found = match rx.try_recv() {
            Ok(found) => found,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            // No store after all: asked for.
            Err(crossbeam_channel::TryRecvError::Disconnected) => None,
        };
        let Some((ask, id, _)) = self.lookup.take() else {
            return;
        };
        match found {
            Some(s) => {
                // From the store: kept there as it is.
                self.give(id, s.clone(), false, ask.from);
                ask.answer(Some(s));
            }
            None => {
                let kind = whole_key(askpass::kind(&ask.prompt), self.target.identity.as_deref());
                self.question = Some(Question::Secret {
                    ask,
                    kind,
                    id: Some(id),
                    input: Zeroizing::default(),
                    remember: false,
                    again: false,
                });
            }
        }
    }
}

impl Drop for Connecting {
    fn drop(&mut self) {
        // A connection left unfinished is stopped; an opened one belongs to its scan now.
        self.cancel();
    }
}

/// `kind` with the key named in full: ssh cuts a long path short in its question, so a key
/// whose path starts with the one asked about is the `identity` chosen.
fn whole_key(kind: Kind, identity: Option<&Path>) -> Kind {
    match (kind, identity.and_then(Path::to_str)) {
        (Kind::Passphrase { key }, Some(id)) if id != key && id.starts_with(key.as_str()) => {
            Kind::Passphrase { key: id.into() }
        }
        (kind, _) => kind,
    }
}

/// The front end the bootstrap asks: the window, through the events.
struct Window {
    events: Sender<Event>,
    ctx: egui::Context,
}

impl Front for Window {
    fn allow(&mut self, what: &str, dir: &str) -> std::io::Result<()> {
        let (reply, answer) = crossbeam_channel::bounded(1);
        let send = Event::Send {
            what: what.into(),
            dir: dir.into(),
            reply,
        };
        let declined = || {
            std::io::Error::other(format!(
                "no dfdu {} agent there, and none was sent",
                env!("CARGO_PKG_VERSION")
            ))
        };
        self.events.send(send).map_err(|_| declined())?;
        self.ctx.request_repaint();
        match answer.recv() {
            Ok(true) => Ok(()),
            _ => Err(declined()),
        }
    }

    fn note(&mut self, msg: &str) {
        let _ = self.events.send(Event::Note(msg.into()));
        self.ctx.request_repaint();
    }
}

/// Connects to `target` with ssh's environment `env` and opens its scan: on the thread.
fn connect(
    target: &Target,
    env: Vec<(OsString, OsString)>,
    rules: Option<dfdu_engine::RemoteRules>,
    opts: dfdu_engine::EngineOptions,
    front: &mut Window,
    cancel: &Cancel,
) -> Result<remote::Opened, String> {
    let ssh = remote::Ssh {
        port: target.port,
        identity: target.identity.clone(),
        env,
        ..remote::Ssh::new(
            target.host.clone(),
            AgentMode::Auto,
            remote::fetch::Agents::from_env(local_cli()),
        )
    };
    let io = remote::connect(&ssh, front, cancel).map_err(|e| e.to_string())?;
    front.note(&format!("Starting the scan of {}…", target.host));
    let root = if target.path.trim().is_empty() {
        Path::new("~")
    } else {
        Path::new(target.path.trim())
    };
    let job = dfdu_scan::JobOptions::default();
    let mut opened = remote::open(io, root, rules, opts, job).map_err(|e| e.to_string())?;
    for msg in std::mem::take(&mut opened.errors) {
        eprintln!("dfdu-gui: {}: {msg}", target.host);
    }
    Ok(opened)
}

/// The program ssh runs as its askpass helper: this one. On Linux a program replaced while it
/// runs (by an update) is named `… (deleted)`; the file now at its path is the one to run.
fn helper() -> std::io::Result<OsString> {
    Ok(live(std::env::current_exe()?, |p| p.is_file()))
}

/// `exe`, or the file now at its path when it was replaced (`is_file` says it is there).
fn live(exe: PathBuf, is_file: impl Fn(&Path) -> bool) -> OsString {
    if let Some(now) = exe.to_str().and_then(|p| p.strip_suffix(" (deleted)"))
        && is_file(Path::new(now))
    {
        return now.into();
    }
    exe.into_os_string()
}

/// The dfdu command-line tool on this computer when it is this version, to send to a host of
/// the same platform: before the release when this program is a build hosts run as is (macOS),
/// after it when it is a glibc build (Linux), which may not run on an older host.
fn local_cli() -> Option<(PathBuf, remote::fetch::Own)> {
    let found = crate::update::install::find_cli()?;
    let want = format!("dfdu {}", env!("CARGO_PKG_VERSION"));
    (found.version.as_deref() == Some(want.as_str()))
        .then_some(found.path)
        .zip(remote::fetch::own())
}

#[cfg(test)]
mod tests;
