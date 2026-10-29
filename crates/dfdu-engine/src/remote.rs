//! The local side of a remote scan: the applier's scanner calls go
//! to a `dfdu agent` as [`ToAgent`] frames, and a reader thread puts the agent's batches into the
//! applier's inbox as if local workers had sent them. The inbox's byte budget blocks the
//! reader, which stops reading the pipe, which blocks the agent's workers.
//!
//! Sequence numbers are the one thing translated: the applier compares a job's submitted
//! number with the one in a `Quiescent`, and the agent counts its own. Each start gets a local
//! number at once and the agent's in [`FromAgent::Started`], in the same order; a `Quiescent` is
//! passed on with the highest local number whose agent number it covers.
//!
//! Classification runs on the agent, with this side's rules: [`connect`] sends their text, the
//! agent builds them for its host and says how, and this side builds the same set from that,
//! to read the classes and to work out what rescans classify against.
//!
//! Deletion runs on the agent ([`spawn_deleter`]): the applier's plans go to the agent's deleter,
//! which verifies and journals on its host, and its results come back into the applier's
//! deleter channel. A result naming a node not in its plan is dropped.
//!
//! Extent measurement runs on the agent too ([`spawn_measurer`]), with the agent's smallest
//! file worth measuring ([`Connected::measure_min`]); results for files not asked about are
//! dropped, and those not answered when the link ends count as unmeasured.
//!
//! The link is dead when one read waits longer than [`DEAD`] (the agent sends a heartbeat
//! every second); time spent not reading, such as while the budget is full, does not count.
//! A dead or closed link ends the session's scanning: once everything that arrived is applied,
//! the running jobs stop where they are, the scan is incomplete and
//! [`Status::disconnected`](crate::Status) says why; nothing more is scanned.
//!
//! The other end is [`agent`]; the frames between them are [`wire`]'s.

pub mod agent;
pub mod wire;

use std::collections::{HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Inode;
use crate::measure::{MeasureMsg, MeasureReq, MeasureResult};
use crate::nanos::nanos;
use crate::threads::spawn;
use crossbeam_channel::{RecvTimeoutError, Sender};
use dfdu_clean::{DeleterHandle, DeleterMsg, DeleterReq};
use dfdu_rules::hash::FastMap as HashMap;
use dfdu_rules::{Env, Platform, RuleSet, Source};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Batch, DirId, JobId, JobOptions, ScanMsg, Sink};
use dfdu_tree::NodeRef;
use parking_lot::Mutex;
use wire::{FromAgent, ToAgent, WireClass, read_frame, write_frame};

/// A read that waits this long without a byte means the agent or the link is gone.
pub const DEAD: Duration = Duration::from_secs(10);

/// How the local side reaches an agent: its stdout, its stdin, and the process to kill when
/// the link is dead (the `ssh` running it).
pub struct RemoteIo {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Option<Child>,
}

/// The rules a remote scan classifies with (rule files in load order, as
/// [`dfdu_rules::sources`] reads them), and the scan root on the agent's host.
pub struct RemoteRules {
    pub files: Vec<(Source, String)>,
    pub root: PathBuf,
}

/// What a connected agent said.
pub struct Connected {
    /// Its program and version.
    pub tool: String,
    /// What the scan root classifies against, with the rules as built for the agent's host;
    /// `None` without rules, or when the two sides do not build the same set.
    pub class: Option<ClassOpts>,
    /// Problems with the rules as built for the agent's host.
    pub errors: Vec<String>,
    /// The smallest allocation the agent measures; 0 when it cannot measure.
    pub measure_min: u64,
    /// The agent runs as root.
    pub privileged: bool,
}

pub(crate) struct Link {
    writer: Mutex<Box<dyn Write + Send>>,
    seq: AtomicU64,
    /// Local numbers of starts the agent has not confirmed yet, oldest first.
    pending: Mutex<VecDeque<u64>>,
    /// `(agent, local)` numbers of confirmed starts, increasing.
    pairs: Mutex<Vec<(u64, u64)>>,
    child: Mutex<Option<Child>>,
    dead: AtomicBool,
    /// The watchdog ended the link.
    timed_out: AtomicBool,
    /// The rules both sides classify with, once agreed.
    rules: OnceLock<Arc<RuleSet>>,
    /// Why the link ended, until the applier takes it.
    lost: Mutex<Option<String>>,
    /// The items of each plan sent and not answered yet, with their bytes as scanned.
    plans: Mutex<HashMap<u64, HashMap<NodeRef, u64>>>,
    /// Native commands sent and not finished yet.
    natives: Mutex<HashSet<u64>>,
    /// Files sent to be measured and not answered yet, by round, node and inode, with how
    /// many times each was asked for.
    measuring: Mutex<HashMap<(u64, NodeRef, Inode), u32>>,
}

impl Link {
    fn send(&self, d: &ToAgent) {
        if self.dead.load(SeqCst) {
            return;
        }
        let mut w = self.writer.lock();
        if write_frame(&mut *w, &d.encode())
            .and_then(|()| w.flush())
            .is_err()
        {
            self.dead.store(true, SeqCst);
        }
    }

    pub(crate) fn start_job(&self, job: JobId, root: &Path, opts: &JobOptions) -> u64 {
        let seq = self.seq.fetch_add(1, SeqCst) + 1;
        self.pending.lock().push_back(seq);
        self.send(&ToAgent::StartJob {
            job,
            root: dfdu_tree::name_bytes(root.as_os_str()).into_owned(),
            cross_mounts: opts.cross_mounts,
            network: opts.network,
            enter: opts.enter.clone(),
            class: opts
                .class
                .as_ref()
                .filter(|_| self.rules.get().is_some())
                .map(WireClass::of),
        });
        seq
    }

    pub(crate) fn end_job(&self, job: JobId) {
        self.send(&ToAgent::EndJob { job });
    }

    pub(crate) fn stop_dirs(&self, job: JobId, dirs: impl IntoIterator<Item = DirId>) {
        let dirs: Vec<DirId> = dirs.into_iter().collect();
        if !dirs.is_empty() {
            self.send(&ToAgent::StopDirs { job, dirs });
        }
    }

    /// The agent retries on its side, so the local side never has to.
    pub(crate) fn abandon(&self, job: JobId, dir: DirId) -> bool {
        self.send(&ToAgent::Abandon { job, dir });
        true
    }

    /// Ends the link: the agent's stdin closes (it exits) and its process is killed.
    pub(crate) fn close(&self) {
        self.dead.store(true, SeqCst);
        *self.writer.lock() = Box::new(io::sink());
        self.kill();
    }

    fn kill(&self) {
        self.dead.store(true, SeqCst);
        if let Some(c) = self.child.lock().as_mut() {
            let _ = c.kill();
        }
    }

    /// Whether a deleter result is about a plan sent, and an item of it not answered yet; a
    /// plan's entry goes with its `Done`.
    fn accept(&self, msg: &DeleterMsg) -> bool {
        let mut plans = self.plans.lock();
        match msg {
            DeleterMsg::Item { plan, node, .. } => plans
                .get_mut(plan)
                .is_some_and(|items| items.remove(node).is_some()),
            DeleterMsg::Done { plan, .. } => plans.remove(plan).is_some(),
            DeleterMsg::NativeOutput { id, .. } => self.natives.lock().contains(id),
            DeleterMsg::NativeDone { id, .. } => self.natives.lock().remove(id),
            _ => true,
        }
    }

    /// Answers the items of plans the agent did not finish before the link ended: whether
    /// they were deleted is not known, so each counts as failed, and its plan ends.
    fn end_plans(&self, deleted: &Sender<DeleterMsg>) {
        let plans = std::mem::take(&mut *self.plans.lock());
        for (plan, items) in plans {
            for (node, bytes) in items {
                let _ = deleted.send(DeleterMsg::Item {
                    plan,
                    node,
                    bytes,
                    result: dfdu_clean::DeleteResult::Failed {
                        partial: true,
                        err: dfdu_tree::ErrKind::Other,
                        msg: "the link to the agent ended: it may be deleted, rescan to see".into(),
                    },
                });
            }
            let _ = deleted.send(DeleterMsg::Done { plan, btrfs: false });
        }
        let natives = std::mem::take(&mut *self.natives.lock());
        for id in natives {
            let _ = deleted.send(DeleterMsg::NativeDone {
                id,
                result: Err("the link to the agent ended: it may have run, rescan to see".into()),
            });
        }
    }

    /// Whether a measurement answers a file asked about and not answered yet.
    fn accept_measured(&self, m: &MeasureMsg) -> bool {
        let mut pending = self.measuring.lock();
        let key = (m.round, m.node, m.inode);
        match pending.get_mut(&key) {
            Some(1) => pending.remove(&key).is_some(),
            Some(n) => {
                *n -= 1;
                true
            }
            None => false,
        }
    }

    /// Answers the files not measured before the link ended: they count as unmeasured.
    fn end_measures(&self, measured: &Sender<Vec<MeasureMsg>>) {
        let pending = std::mem::take(&mut *self.measuring.lock());
        let msgs: Vec<MeasureMsg> = pending
            .into_iter()
            .flat_map(|((round, node, inode), n)| {
                (0..n).map(move |_| MeasureMsg {
                    round,
                    node,
                    inode,
                    result: MeasureResult::Failed(dfdu_tree::ErrKind::Other),
                })
            })
            .collect();
        if !msgs.is_empty() {
            let _ = measured.send(msgs);
        }
    }

    /// Why the link ended, once; set after the reader's last batch went into the inbox.
    pub(crate) fn take_lost(&self) -> Option<String> {
        self.lost.lock().take()
    }

    /// The local number a `Quiescent` with the agent's number `agent` stands for.
    fn local_seq(&self, agent: u64) -> u64 {
        self.pairs
            .lock()
            .iter()
            .rev()
            .find(|(a, _)| *a <= agent)
            .map_or(0, |&(_, l)| l)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A reader that records how long its current read has waited.
struct Timed<R> {
    inner: R,
    start: Instant,
    /// Nanoseconds after `start` when the current read began; 0 when not reading.
    since: Arc<AtomicU64>,
}

impl<R: Read> Read for Timed<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.since.store(nanos(self.start.elapsed()) + 1, SeqCst);
        let r = self.inner.read(buf);
        self.since.store(0, SeqCst);
        r
    }
}

/// Greets the agent: the versions must overlap. With `rules`, sends them and builds the set the
/// agent built. Returns the link and the agent's reader.
pub(crate) fn connect(
    io: RemoteIo,
    rules: Option<RemoteRules>,
) -> io::Result<(Arc<Link>, Box<dyn Read + Send>, Connected)> {
    let RemoteIo {
        mut reader,
        writer,
        child,
    } = io;
    let mut buf = Vec::new();
    let hello = next_message(
        &mut reader,
        &mut buf,
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the agent closed the link before saying hello",
        ),
    )?;
    let tool = match hello {
        FromAgent::Hello { min, max, tool } if (min..=max).contains(&wire::VERSION) => tool,
        FromAgent::Hello { min, max, tool } => {
            return Err(io::Error::other(format!(
                "{tool} speaks protocol versions {min}-{max}, this dfdu {}",
                wire::VERSION
            )));
        }
        FromAgent::Fatal(s) => return Err(io::Error::other(s)),
        _ => return Err(io::Error::other("the agent did not say hello")),
    };
    let link = Arc::new(Link {
        writer: Mutex::new(writer),
        seq: AtomicU64::new(0),
        pending: Mutex::new(VecDeque::new()),
        pairs: Mutex::new(Vec::new()),
        child: Mutex::new(child),
        dead: AtomicBool::new(false),
        timed_out: AtomicBool::new(false),
        rules: OnceLock::new(),
        lost: Mutex::new(None),
        plans: Mutex::new(HashMap::default()),
        natives: Mutex::new(HashSet::default()),
        measuring: Mutex::new(HashMap::default()),
    });
    link.send(&ToAgent::Hello {
        version: wire::VERSION,
    });
    let (measure_min, privileged) = loop {
        let closed = io::Error::other("the agent closed the link");
        match next_message(&mut reader, &mut buf, closed)? {
            FromAgent::Heartbeat => {}
            FromAgent::Fatal(s) => return Err(io::Error::other(s)),
            FromAgent::Host {
                measure_min,
                privileged,
            } => break (measure_min, privileged),
            _ => return Err(io::Error::other("the agent did not describe its host")),
        }
    };
    let mut connected = Connected {
        tool,
        class: None,
        errors: Vec::new(),
        measure_min,
        privileged,
    };
    if let Some(r) = rules {
        exchange_rules(&link, &mut reader, &mut buf, &r, &mut connected)?;
    }
    if link.dead.load(SeqCst) {
        return Err(io::Error::other("the agent closed the link"));
    }
    Ok((link, reader, connected))
}

/// Sends `r` to the agent and builds the set it built from them, into `connected`; rules the
/// two sides do not build alike are left out, with why in `connected.errors`.
fn exchange_rules(
    link: &Link,
    reader: &mut impl Read,
    buf: &mut Vec<u8>,
    r: &RemoteRules,
    connected: &mut Connected,
) -> io::Result<()> {
    link.send(&ToAgent::Rules {
        files: r
            .files
            .iter()
            .map(|(src, text)| {
                let name = match src {
                    Source::Builtin => None,
                    Source::File(p) => Some(p.display().to_string()),
                };
                (name, text.clone())
            })
            .collect(),
        root: dfdu_tree::name_bytes(r.root.as_os_str()).into_owned(),
    });
    let answer = loop {
        let closed = io::Error::other("the agent closed the link");
        match next_message(reader, buf, closed)? {
            // Before a job starts, batches only say the agent's workers are idle.
            FromAgent::Heartbeat | FromAgent::Batch(_) => {}
            FromAgent::Fatal(s) => return Err(io::Error::other(s)),
            FromAgent::Rules {
                platform,
                vars,
                links,
                fingerprint,
                root,
            } => break (platform, vars, links, fingerprint, root),
            _ => return Err(io::Error::other("the agent did not answer the rules")),
        }
    };
    let (platform, vars, links, fingerprint, root) = answer;
    let built = Built {
        platform: &platform,
        vars: &vars,
        links: &links,
    };
    match agree(&r.files, &built, fingerprint, &root) {
        Ok((set, errors)) => {
            let set = Arc::new(set);
            let _ = link.rules.set(set.clone());
            connected.class = Some(root.with(set));
            connected.errors = errors;
        }
        Err(e) => connected.errors.push(e),
    }
    Ok(())
}

/// The next message from the agent while it is greeted, decoded without rules; `closed` if
/// the link ends first.
fn next_message(
    reader: &mut impl Read,
    buf: &mut Vec<u8>,
    closed: io::Error,
) -> io::Result<FromAgent> {
    let bad = |e: wire::WireError| io::Error::new(io::ErrorKind::InvalidData, e);
    if !read_frame(reader, buf).map_err(bad)? {
        return Err(closed);
    }
    FromAgent::decode(buf, None).map_err(bad)
}

/// What the agent built its rules for ([`FromAgent::Rules`]).
struct Built<'a> {
    platform: &'a str,
    vars: &'a [(String, String)],
    /// The path patterns that lead elsewhere through a link on the agent's disks.
    links: &'a [(String, String)],
}

/// Builds `files` as the agent did, for its platform, variables and links; links on this
/// machine's disks are never looked at, since the paths are the agent's. The set must have the
/// agent's `fingerprint` and name every rule `root` does. Returns the set and its load errors.
fn agree(
    files: &[(Source, String)],
    built: &Built,
    fingerprint: u64,
    root: &WireClass,
) -> Result<(RuleSet, Vec<String>), String> {
    let platform = built.platform;
    let p = Platform::parse(platform).ok_or_else(|| {
        format!("the agent runs on an unknown platform `{platform}`: not classified")
    })?;
    let vars: Vec<(&str, &str)> = (built.vars.iter())
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let env = Env::with(p, &vars).with_links(built.links);
    let (set, errors) = dfdu_rules::load_from(&env, files);
    if set.fingerprint() != fingerprint || !root.fits(&set) {
        return Err(
            "the agent built different rules (another dfdu version?): not classified".into(),
        );
    }
    Ok((
        set,
        errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    ))
}

/// A deleter whose plans run on the agent: requests are sent as they come, and an abort is
/// passed on. Results arrive through the reader ([`spawn_reader`]'s `deleted`); a plan sent
/// on a dead link is answered into `deleted` here.
pub(crate) fn spawn_deleter(link: Arc<Link>, deleted: Sender<DeleterMsg>) -> DeleterHandle {
    let (reqs, rx) = crossbeam_channel::unbounded::<DeleterReq>();
    let abort = Arc::new(AtomicBool::new(false));
    let ab = abort.clone();
    let thread = spawn("dfdu-remote-del", move || {
        loop {
            if ab.swap(false, SeqCst) {
                link.send(&ToAgent::AbortDelete);
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(DeleterReq::Plan(p)) => {
                    link.plans
                        .lock()
                        .insert(p.id, p.items.iter().map(|i| (i.node, i.bytes)).collect());
                    // A plan starting clears an abort asked for before it, as locally.
                    ab.store(false, SeqCst);
                    link.send(&ToAgent::Delete(DeleterReq::Plan(p)));
                    if link.dead.load(SeqCst) {
                        link.end_plans(&deleted);
                    }
                }
                Ok(DeleterReq::Native(n)) => {
                    if n.rule.is_none() {
                        let _ = deleted.send(DeleterMsg::NativeDone {
                            id: n.id,
                            result: Err("not from a rule: not sent to the agent".into()),
                        });
                        continue;
                    }
                    link.natives.lock().insert(n.id);
                    link.send(&ToAgent::Delete(DeleterReq::Native(n)));
                    if link.dead.load(SeqCst) {
                        link.end_plans(&deleted);
                    }
                }
                Ok(r) => link.send(&ToAgent::Delete(r)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    DeleterHandle {
        reqs,
        abort,
        thread,
    }
}

/// Guard checks and native sizes run on the agent: each batch is sent as it comes; the answers
/// arrive through the reader ([`spawn_reader`]'s `checked`).
pub(crate) fn spawn_checker(link: Arc<Link>) -> Sender<Vec<CheckReq>> {
    let (tx, rx) = crossbeam_channel::unbounded::<Vec<CheckReq>>();
    spawn("dfdu-remote-check", move || {
        for batch in rx {
            if !batch.is_empty() {
                link.send(&ToAgent::Check(batch));
            }
        }
    });
    tx
}

/// Extent measurement on the agent: each slice's requests are sent as one frame; the answers
/// arrive through the reader ([`spawn_reader`]'s `measured`), and requests sent on a dead
/// link are answered into `measured` here.
pub(crate) fn spawn_measurer(
    link: Arc<Link>,
    measured: Sender<Vec<MeasureMsg>>,
) -> Sender<MeasureReq> {
    let (tx, rx) = crossbeam_channel::unbounded::<MeasureReq>();
    spawn("dfdu-remote-measure", move || {
        while let Ok(first) = rx.recv() {
            let reqs: Vec<MeasureReq> = std::iter::once(first).chain(rx.try_iter()).collect();
            {
                let mut pending = link.measuring.lock();
                for r in &reqs {
                    for f in &r.files {
                        *pending.entry((r.round, f.node, f.inode)).or_default() += 1;
                    }
                }
            }
            link.send(&ToAgent::Measure(reqs));
            if link.dead.load(SeqCst) {
                link.end_measures(&measured);
            }
        }
    });
    tx
}

/// The channels the reader passes the agent's answers into.
pub(crate) struct Answers {
    pub deleted: Sender<DeleterMsg>,
    pub checked: Sender<Vec<CheckMsg>>,
    pub measured: Sender<Vec<MeasureMsg>>,
}

/// Starts the threads that read the agent's frames into `sink` (the deleter's results, guard
/// checks and measurements into `answers`) and watch the link; when the link ends, records
/// why ([`Link::take_lost`]) and calls `on_end`.
pub(crate) fn spawn_reader<S: Sink + Send + Sync + 'static>(
    link: Arc<Link>,
    reader: Box<dyn Read + Send>,
    sink: Arc<S>,
    answers: Answers,
    stop: Arc<AtomicBool>,
    on_end: impl FnOnce() + Send + 'static,
) {
    let since = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let mut r = Timed {
        inner: reader,
        start,
        since: since.clone(),
    };
    {
        let (link, stop, since) = (link.clone(), stop.clone(), since.clone());
        spawn("dfdu-link-watch", move || {
            while !stop.load(SeqCst) && !link.dead.load(SeqCst) {
                thread::sleep(Duration::from_millis(100));
                let s = since.load(SeqCst);
                if s != 0 && nanos(start.elapsed()) - s > nanos(DEAD) {
                    link.timed_out.store(true, SeqCst);
                    link.kill();
                }
            }
        });
    }
    spawn("dfdu-link", move || {
        let why = pump(&link, &mut r, &*sink, &answers);
        link.dead.store(true, SeqCst);
        link.end_plans(&answers.deleted);
        link.end_measures(&answers.measured);
        if !stop.load(SeqCst) {
            *link.lost.lock() = Some(why);
            on_end();
        }
    });
}

/// Reads frames until the link ends; returns why it ended.
fn pump(link: &Link, r: &mut impl Read, sink: &impl Sink, answers: &Answers) -> String {
    let mut buf = Vec::new();
    loop {
        match read_frame(r, &mut buf) {
            Ok(true) => {}
            Ok(false) if link.timed_out.load(SeqCst) => return "no data for 10 s".into(),
            Ok(false) => return "the agent closed the link".into(),
            Err(e) => return e.to_string(),
        }
        match FromAgent::decode(&buf, link.rules.get().map(|r| &**r)) {
            Ok(FromAgent::Batch(mut msgs)) => {
                let mut bytes = 0;
                for m in &mut msgs {
                    if let ScanMsg::Quiescent { submitted_seq } = m {
                        *submitted_seq = link.local_seq(*submitted_seq);
                    }
                    bytes += m.size();
                }
                sink.send(Batch { msgs, bytes });
            }
            Ok(FromAgent::Started { seq, .. }) => {
                if let Some(local) = link.pending.lock().pop_front() {
                    link.pairs.lock().push((seq, local));
                }
            }
            Ok(
                FromAgent::Heartbeat
                | FromAgent::Abandoned { .. }
                | FromAgent::Hello { .. }
                | FromAgent::Rules { .. }
                | FromAgent::Host { .. },
            ) => {}
            Ok(FromAgent::Checked(msgs)) => {
                let _ = answers.checked.send(msgs);
            }
            Ok(FromAgent::Deleter(msg)) => {
                if link.accept(&msg) {
                    let _ = answers.deleted.send(msg);
                }
            }
            Ok(FromAgent::Measured(mut msgs)) => {
                msgs.retain(|m| link.accept_measured(m));
                if !msgs.is_empty() {
                    let _ = answers.measured.send(msgs);
                }
            }
            Ok(FromAgent::Fatal(s)) => return format!("the agent stopped: {s}"),
            Err(e) => return e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set built from the agent's answer must be the agent's: another platform's rules,
    /// or rules the agent does not number alike, classify nothing.
    #[test]
    fn rules_are_agreed_or_dropped() {
        let files = vec![(Source::Builtin, dfdu_rules::load::BUILTIN.to_owned())];
        let vars = vec![("HOME".to_owned(), "/home/u".to_owned())];
        let env = Env::with(Platform::Linux, &[("HOME", "/home/u")]);
        let set = dfdu_rules::load_from(&env, &files).0;
        let root = WireClass {
            path: b"/home/u/p".to_vec(),
            inside: vec![],
            protection: dfdu_rules::protected::State::default(),
            chain: vec![],
        };
        let fp = set.fingerprint();
        let on = |platform| Built {
            platform,
            vars: &vars,
            links: &[],
        };
        let (got, errors) = agree(&files, &on("linux"), fp, &root).unwrap();
        assert_eq!(got.fingerprint(), fp);
        assert!(errors.is_empty(), "{errors:?}");
        // Another platform builds other rules.
        assert_ne!(
            dfdu_rules::load_from(&Env::with(Platform::MacOs, &[]), &files)
                .0
                .fingerprint(),
            fp
        );
        assert!(agree(&files, &on("macos"), fp, &root).is_err());
        assert!(agree(&files, &on("plan9"), fp, &root).is_err());
        assert!(agree(&files, &on("linux"), fp ^ 1, &root).is_err());
        // A path pattern the agent followed through a link: built alike only with the link.
        let links = [(
            "/home/u/.cargo/registry".to_owned(),
            "/mnt/data/cargo/registry".to_owned(),
        )];
        let linked = Built {
            links: &links,
            ..on("linux")
        };
        let agent = dfdu_rules::load_from(&env.clone().with_links(&links), &files).0;
        let lfp = agent.fingerprint();
        assert_ne!(lfp, fp);
        assert!(agree(&files, &on("linux"), lfp, &root).is_err());
        assert_eq!(
            agree(&files, &linked, lfp, &root).unwrap().0.fingerprint(),
            lfp
        );
        let unknown = WireClass {
            inside: vec![dfdu_rules::RuleIx(u16::MAX)],
            ..root
        };
        assert!(agree(&files, &on("linux"), fp, &unknown).is_err());
    }
}
