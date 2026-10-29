//! `dfdu agent`: the scanner run for a local dfdu on another host, over the agent's stdin and
//! stdout (the frames are [`super::wire`]'s).
//!
//! The agent runs the scanner alone (workers, watchdog, classification with the rules the local
//! side sent, [`ToAgent::Rules`]) and sends what it produces as [`FromAgent::Batch`] frames;
//! the local side's applier builds the tree. Deletion runs here too: plans go to a deleter of
//! the agent's own, which verifies each item against this host's disk and journals it in this
//! host's journal directory, and its results go back as [`FromAgent::Deleter`]; guard checks
//! and extent measurement run here as well. Writes block when the local side stops reading,
//! which stops the workers: the pipe is the byte budget. A heartbeat goes out after a second
//! with nothing sent. The agent exits when its stdin ends or its stdout fails (the local side
//! is gone; its stdin may stay open, as under a shell).

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender, bounded, unbounded};
use parking_lot::Mutex;

use dfdu_clean::{DeleterHandle, DeleterMsg, DeleterReq, STAGE_UNDO_WINDOW, spawn_deleter};
use dfdu_rules::{Env, RuleSet, Source};

use super::wire::{FromAgent, ToAgent, VERSION, WireClass, read_frame, write_frame};
use crate::classifier::CheckReq;
use crate::measure::{MeasureMsg, MeasureReq, spawn_measurers};
use crate::nanos::nanos;
use crate::threads::spawn;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::runtime::Pool;
use dfdu_scan::{Batch, DirId, JobId, JobOptions, Limits, ScanMsg, Shared, Sink};

/// How long the agent may send nothing before it sends a heartbeat.
pub const HEARTBEAT: Duration = Duration::from_secs(1);

/// The agent's stdout: frames from the workers, the control loop and the heartbeat, one at a
/// time.
struct Out<W> {
    w: Mutex<W>,
    start: Instant,
    /// Nanoseconds after `start` of the last frame written.
    last: AtomicU64,
    failed: AtomicBool,
}

impl<W: Write> Out<W> {
    fn send(&self, up: &FromAgent) {
        let payload = up.encode();
        let mut w = self.w.lock();
        if write_frame(&mut *w, &payload)
            .and_then(|()| w.flush())
            .is_err()
        {
            self.failed.store(true, SeqCst);
        }
        self.last.store(nanos(self.start.elapsed()), SeqCst);
    }
}

impl<W: Write + Send> Sink for Out<W> {
    fn try_send(&self, batch: Batch) -> Result<(), Batch> {
        self.send(&FromAgent::Batch(batch.msgs));
        Ok(())
    }
    fn send(&self, batch: Batch) {
        Out::send(self, &FromAgent::Batch(batch.msgs));
    }
    fn send_control(&self, msg: ScanMsg) {
        Out::send(self, &FromAgent::Batch(vec![msg]));
    }
}

/// Runs the agent with `backend` and `workers` worker threads until `input` ends or the
/// output fails. Deletions are journalled in `journal_dir`; without one, every item is refused.
///
/// # Errors
///
/// If the local side speaks another protocol version or sends something that is not a valid
/// message, or the output fails.
pub fn run<B: dfdu_scan::Backend>(
    backend: B,
    workers: usize,
    mut journal_dir: Option<PathBuf>,
    mut input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> io::Result<()> {
    let out = Arc::new(Out {
        w: Mutex::new(output),
        start: Instant::now(),
        last: AtomicU64::new(0),
        failed: AtomicBool::new(false),
    });
    if !greet(&out, &mut input)? {
        return Ok(());
    }
    let bad = |e: super::wire::WireError| io::Error::new(io::ErrorKind::InvalidData, e);
    let measurers = describe_host(&out);
    let shared = Arc::new(Shared::new(
        backend,
        Limits {
            chunk_credit: workers.max(2),
            pool_workers: workers.max(1),
            ..Limits::default()
        },
    ));
    let pool = Pool::start(&shared, &out, workers);
    let done = Arc::new(AtomicBool::new(false));
    let heart = start_heartbeat(&out, &done);
    let rx = start_input(input);
    let mut rules: Option<Arc<RuleSet>> = None;
    let mut deleter: Option<DeleterHandle> = None;
    let mut measure: Option<Sender<MeasureReq>> = None;
    let result = loop {
        if out.failed.load(SeqCst) {
            break Err(io::Error::other("output closed"));
        }
        let buf = match rx.recv_timeout(HEARTBEAT / 4) {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => break Err(bad(e)),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        };
        let down = match ToAgent::decode(&buf, rules.as_deref()) {
            Ok(d) => d,
            Err(e) => break Err(bad(e)),
        };
        match down {
            ToAgent::Hello { .. } => {}
            ToAgent::Rules { files, root } => {
                let (set, answer) = build_rules(&files, &root);
                rules = Some(set);
                out.send(&answer);
            }
            ToAgent::StartJob {
                job,
                root,
                cross_mounts,
                network,
                enter,
                class,
            } => {
                let root = PathBuf::from(dfdu_tree::name_os(&root).into_owned());
                // Decoding refuses a class before the rules.
                let class = class.zip(rules.clone()).map(|(c, r)| c.with(r));
                let opts = job_options(cross_mounts, network, enter, class);
                let seq = shared.start_job(job, &root, opts);
                out.send(&FromAgent::Started { job, seq });
            }
            ToAgent::Delete(mut req) => {
                // The protected paths of the rules this host classified with, as for a local
                // plan; the other side's are for its own environment.
                if let DeleterReq::Plan(p) = &mut req {
                    p.protected = rules.as_ref().map(|r| r.protected().clone());
                }
                // Created once: the journal directory is the deleter's from then on.
                let d = deleter.get_or_insert_with(|| start_deleter(&out, journal_dir.take()));
                let _ = d.reqs.send(req);
            }
            ToAgent::Check(reqs) => {
                if let Err(e) = check(&out, reqs) {
                    break Err(e);
                }
            }
            ToAgent::Measure(reqs) => {
                let m = measure.get_or_insert_with(|| start_measurers(&out, measurers));
                for r in reqs {
                    let _ = m.send(r);
                }
            }
            ToAgent::AbortDelete => {
                if let Some(d) = &deleter {
                    d.abort.store(true, SeqCst);
                }
            }
            ToAgent::EndJob { job } => shared.end_job(job),
            ToAgent::StopDirs { job, dirs } => shared.stop_dirs(job, dirs),
            ToAgent::Abandon { job, dir } => {
                let complete = abandon(&shared, job, dir);
                out.send(&FromAgent::Abandoned { job, dir, complete });
            }
        }
    };
    done.store(true, SeqCst);
    // The measurers end with their requests; one stuck in a filesystem call is left behind.
    drop(measure);
    if let Some(d) = deleter {
        stop_deleter(d);
    }
    let _ = heart.join();
    pool.stop(Duration::from_millis(200));
    result
}

/// Answers guard checks on a thread of their own: they read git indexes and the process list,
/// off the control loop.
fn check<W: Write + Send + 'static>(out: &Arc<Out<W>>, reqs: Vec<CheckReq>) -> io::Result<()> {
    let out = out.clone();
    thread::Builder::new()
        .name("dfdu-agent-check".into())
        .spawn(move || out.send(&FromAgent::Checked(crate::check_batch(&reqs))))
        .map(drop)
}

/// Abandons directory `dir` of job `job`; whether it is complete (no worker inside it is left
/// to give its outbox up), waiting up to about 100 ms for one that will soon.
fn abandon<B: dfdu_scan::Backend>(shared: &Shared<B>, job: JobId, dir: DirId) -> bool {
    let mut complete = shared.abandon(job, dir);
    for _ in 0..100 {
        if complete {
            break;
        }
        thread::sleep(Duration::from_millis(1));
        complete = shared.abandon(job, dir);
    }
    complete
}

/// Stops the agent's deleter: a plan still running stops after its current item; staged items
/// are left for recovery, as when a local session ends.
fn stop_deleter(d: DeleterHandle) {
    d.abort.store(true, SeqCst);
    drop(d.reqs);
    let _ = d.thread.join();
}

/// Tells the local side what this host can measure and whether the agent runs privileged;
/// returns the number of measurers to start.
fn describe_host<W: Write>(out: &Out<W>) -> usize {
    let measurers = crate::runtime::default_measurers();
    out.send(&FromAgent::Host {
        measure_min: if measurers > 0 {
            crate::applier::MEASURE_MIN
        } else {
            0
        },
        privileged: dfdu_sys::is_privileged(),
    });
    measurers
}

/// Exchanges hellos with the local side; `false` if `input` ends first.
fn greet<W: Write>(out: &Out<W>, input: &mut impl Read) -> io::Result<bool> {
    out.send(&FromAgent::Hello {
        min: VERSION,
        max: VERSION,
        tool: format!("dfdu {}", env!("CARGO_PKG_VERSION")),
    });
    let mut buf = Vec::new();
    let bad = |e: super::wire::WireError| io::Error::new(io::ErrorKind::InvalidData, e);
    if !read_frame(input, &mut buf).map_err(bad)? {
        return Ok(false);
    }
    match ToAgent::decode(&buf, None).map_err(bad)? {
        ToAgent::Hello { version } if version == VERSION => Ok(true),
        ToAgent::Hello { version } => {
            out.send(&FromAgent::Fatal(format!(
                "protocol version {version} not spoken"
            )));
            Err(io::Error::other("protocol version mismatch"))
        }
        _ => Err(io::Error::other("expected a hello")),
    }
}

/// Sends a heartbeat whenever nothing has gone out for [`HEARTBEAT`], until `done`.
fn start_heartbeat<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    done: &Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let (out, done) = (out.clone(), done.clone());
    spawn("dfdu-heartbeat", move || {
        while !done.load(SeqCst) {
            thread::sleep(HEARTBEAT / 4);
            let idle = nanos(out.start.elapsed()) - out.last.load(SeqCst);
            if idle >= nanos(HEARTBEAT) {
                out.send(&FromAgent::Heartbeat);
            }
        }
    })
}

/// Reads frames on a thread of their own, so a failed output is noticed while a read waits;
/// that thread is left blocked when the agent returns for that reason. The channel ends with
/// `input`, after its first error.
fn start_input(
    mut input: impl Read + Send + 'static,
) -> crossbeam_channel::Receiver<Result<Vec<u8>, super::wire::WireError>> {
    let (tx, rx) = bounded(16);
    spawn("dfdu-agent-in", move || {
        let mut buf = Vec::new();
        loop {
            let r = match read_frame(&mut input, &mut buf) {
                Ok(true) => Ok(std::mem::take(&mut buf)),
                Ok(false) => return,
                Err(e) => Err(e),
            };
            let failed = r.is_err();
            if tx.send(r).is_err() || failed {
                return;
            }
        }
    });
    rx
}

/// A deleter journalling in `journal_dir`, its results sent as they come.
fn start_deleter<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    journal_dir: Option<PathBuf>,
) -> DeleterHandle {
    let (tx, rx) = unbounded::<DeleterMsg>();
    let handle = spawn_deleter(journal_dir, STAGE_UNDO_WINDOW, tx);
    let out = out.clone();
    spawn("dfdu-agent-del", move || {
        for msg in rx {
            out.send(&FromAgent::Deleter(msg));
        }
    });
    handle
}

/// `n` measurers (at least one, which fails every file where nothing can be measured), their
/// results sent as they come.
fn start_measurers<W: Write + Send + 'static>(out: &Arc<Out<W>>, n: usize) -> Sender<MeasureReq> {
    let (tx, rx) = unbounded::<Vec<MeasureMsg>>();
    let reqs = spawn_measurers(n, &tx);
    let out = out.clone();
    spawn("dfdu-agent-measure", move || {
        for msgs in rx {
            out.send(&FromAgent::Measured(msgs));
        }
    });
    reqs
}

/// The options of a job [`ToAgent::StartJob`] asks for.
fn job_options(
    cross_mounts: bool,
    network: bool,
    enter: Vec<(u64, u64)>,
    class: Option<ClassOpts>,
) -> JobOptions {
    JobOptions {
        cross_mounts,
        network,
        enter,
        class,
        ..JobOptions::default()
    }
}

/// The rules in `files` built for this host, and the answer to [`ToAgent::Rules`]: this host's
/// platform, the variables the rules read and the links on its disks their paths go through,
/// their fingerprint, and what `root` classifies against as a scan root.
fn build_rules(files: &[(Option<String>, String)], root: &[u8]) -> (Arc<RuleSet>, FromAgent) {
    let env = Env::current();
    let sources: Vec<(Source, String)> = files
        .iter()
        .map(|(name, text)| {
            let src = name
                .as_ref()
                .map_or(Source::Builtin, |n| Source::File(n.into()));
            (src, text.clone())
        })
        .collect();
    // The local side reports load errors: it builds the same set.
    let (set, _) = dfdu_rules::load_from(&env, &sources);
    let set = Arc::new(set);
    let texts: Vec<&str> = files.iter().map(|(_, t)| t.as_str()).collect();
    let root = PathBuf::from(dfdu_tree::name_os(root).into_owned());
    let answer = FromAgent::Rules {
        platform: env.platform.as_str().into(),
        vars: env.used_by(&texts),
        links: env.links_followed(),
        fingerprint: set.fingerprint(),
        root: WireClass::of(&ClassOpts::for_root(set.clone(), &root)),
    };
    (set, answer)
}
