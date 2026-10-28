//! `dfdu agent`: the scanner run for a local dfdu on another host, over the agent's stdin and
//! stdout (`data-pipeline.md` §9.4; the frames are [`crate::wire`]'s).
//!
//! The agent runs the scanner alone (workers, watchdog, classification with the rules the local
//! side sent, [`Down::Rules`]) and sends what it produces as [`Up::Batch`] frames; the local
//! side's applier builds the tree. Deletion runs here too: plans go to a deleter of the agent's
//! own, which verifies each item against this host's disk and journals it in this host's
//! journal directory, and its results go back as [`Up::Deleter`]; guard checks and extent
//! measurement run here as well. Writes block
//! when the local side stops reading, which stops the workers: the pipe is the byte budget.
//! A heartbeat goes out after a second with nothing sent. The agent exits when its stdin ends
//! or its stdout fails (the local side is gone; its stdin may stay open, as under a shell).

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use dfdu_clean::{DeleterHandle, DeleterMsg, STAGE_UNDO_WINDOW, spawn_deleter};
use dfdu_rules::{Env, RuleSet, Source};

use crate::measure::{MeasureMsg, MeasureReq, spawn_measurers};
use crate::wire::{Down, Up, VERSION, WireClass, read_frame, write_frame};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::runtime::Pool;
use dfdu_scan::{Batch, JobOptions, Limits, ScanMsg, Shared, Sink};

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
    fn send(&self, up: &Up) {
        let payload = up.encode();
        let mut w = self.w.lock();
        if write_frame(&mut *w, &payload)
            .and_then(|()| w.flush())
            .is_err()
        {
            self.failed.store(true, SeqCst);
        }
        self.last
            .store(self.start.elapsed().as_nanos() as u64, SeqCst);
    }
}

impl<W: Write + Send> Sink for Out<W> {
    fn try_send(&self, batch: Batch) -> Result<(), Batch> {
        self.send(&Up::Batch(batch.msgs));
        Ok(())
    }
    fn send(&self, batch: Batch) {
        Out::send(self, &Up::Batch(batch.msgs));
    }
    fn send_control(&self, msg: ScanMsg) {
        Out::send(self, &Up::Batch(vec![msg]));
    }
}

/// Runs the agent with `backend` and `workers` worker threads until `input` ends or the
/// output fails. Deletions are journalled in `journal_dir`; without one, every item is refused.
pub fn run<B: dfdu_scan::Backend>(
    backend: B,
    workers: usize,
    journal_dir: Option<PathBuf>,
    mut input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> io::Result<()> {
    let out = Arc::new(Out {
        w: Mutex::new(output),
        start: Instant::now(),
        last: AtomicU64::new(0),
        failed: AtomicBool::new(false),
    });
    out.send(&Up::Hello {
        min: VERSION,
        max: VERSION,
        tool: format!("dfdu {}", env!("CARGO_PKG_VERSION")),
    });
    let mut buf = Vec::new();
    let bad = |e: crate::wire::WireError| io::Error::new(io::ErrorKind::InvalidData, e);
    if !read_frame(&mut input, &mut buf).map_err(bad)? {
        return Ok(());
    }
    match Down::decode(&buf, None).map_err(bad)? {
        Down::Hello { version } if version == VERSION => {}
        Down::Hello { version } => {
            out.send(&Up::Fatal(format!("protocol version {version} not spoken")));
            return Err(io::Error::other("protocol version mismatch"));
        }
        _ => return Err(io::Error::other("expected a hello")),
    }
    let measurers = crate::runtime::default_measurers();
    out.send(&Up::Host {
        measure_min: if measurers > 0 {
            crate::applier::MEASURE_MIN
        } else {
            0
        },
        privileged: dfdu_sys::is_privileged(),
    });
    let shared = Arc::new(Shared::new(
        backend,
        Limits {
            chunk_credit: workers.max(2),
            pool_workers: workers.max(1),
            ..Limits::default()
        },
    ));
    let pool = Pool::start(shared.clone(), out.clone(), workers);
    let done = Arc::new(AtomicBool::new(false));
    let heart = {
        let (out, done) = (out.clone(), done.clone());
        thread::Builder::new()
            .name("dfdu-heartbeat".into())
            .spawn(move || {
                while !done.load(SeqCst) {
                    thread::sleep(HEARTBEAT / 4);
                    let idle = out.start.elapsed().as_nanos() as u64 - out.last.load(SeqCst);
                    if idle >= HEARTBEAT.as_nanos() as u64 {
                        out.send(&Up::Heartbeat);
                    }
                }
            })
            .expect("spawn heartbeat")
    };
    // Frames are read on a thread of their own, so a failed output is noticed while a read
    // waits; that thread is left blocked when the agent returns for that reason.
    let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>, crate::wire::WireError>>(16);
    thread::Builder::new()
        .name("dfdu-agent-in".into())
        .spawn(move || {
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
        })
        .expect("spawn agent input");
    let mut rules: Option<Arc<RuleSet>> = None;
    let mut deleter: Option<DeleterHandle> = None;
    let mut measure: Option<crossbeam_channel::Sender<MeasureReq>> = None;
    let result = loop {
        if out.failed.load(SeqCst) {
            break Err(io::Error::other("output closed"));
        }
        let buf = match rx.recv_timeout(HEARTBEAT / 4) {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => break Err(bad(e)),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        };
        let down = match Down::decode(&buf, rules.as_deref()) {
            Ok(d) => d,
            Err(e) => break Err(bad(e)),
        };
        match down {
            Down::Hello { .. } => {}
            Down::Rules { files, root } => {
                let (set, answer) = build_rules(&files, &root);
                rules = Some(set);
                out.send(&answer);
            }
            Down::StartJob {
                job,
                root,
                cross_mounts,
                network,
                class,
            } => {
                let root = PathBuf::from(dfdu_tree::name_os(&root).into_owned());
                let opts = JobOptions {
                    cross_mounts,
                    network,
                    // Decoding refuses a class before the rules.
                    class: class.zip(rules.clone()).map(|(c, r)| c.with(r)),
                    ..JobOptions::default()
                };
                let seq = shared.start_job(job, &root, opts);
                out.send(&Up::Started { job, seq });
            }
            Down::Delete(req) => {
                let d = deleter.get_or_insert_with(|| start_deleter(&out, journal_dir.clone()));
                let _ = d.reqs.send(req);
            }
            Down::Check(reqs) => {
                // Guard checks read git indexes and the process list: off the control loop.
                let out = out.clone();
                let spawned = thread::Builder::new()
                    .name("dfdu-agent-check".into())
                    .spawn(move || out.send(&Up::Checked(crate::check_batch(&reqs))));
                if let Err(e) = spawned {
                    break Err(e);
                }
            }
            Down::Measure(reqs) => {
                let m = measure.get_or_insert_with(|| start_measurers(&out, measurers));
                for r in reqs {
                    let _ = m.send(r);
                }
            }
            Down::AbortDelete => {
                if let Some(d) = &deleter {
                    d.abort.store(true, SeqCst);
                }
            }
            Down::EndJob { job } => shared.end_job(job),
            Down::StopDirs { job, dirs } => shared.stop_dirs(job, dirs),
            Down::Abandon { job, dir } => {
                // A worker inside the directory gives its outbox up soon.
                let mut complete = shared.abandon(job, dir);
                for _ in 0..100 {
                    if complete {
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                    complete = shared.abandon(job, dir);
                }
                out.send(&Up::Abandoned { job, dir, complete });
            }
        }
    };
    done.store(true, SeqCst);
    // The measurers end with their requests; one stuck in a filesystem call is left behind.
    drop(measure);
    if let Some(d) = deleter {
        // A plan still running stops after its current item; staged items are left for
        // recovery, as when a local session ends.
        d.abort.store(true, SeqCst);
        drop(d.reqs);
        let _ = d.thread.join();
    }
    let _ = heart.join();
    pool.stop(Duration::from_millis(200));
    result
}

/// A deleter journalling in `journal_dir`, its results sent as they come.
fn start_deleter<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    journal_dir: Option<PathBuf>,
) -> DeleterHandle {
    let (tx, rx) = crossbeam_channel::unbounded::<DeleterMsg>();
    let handle = spawn_deleter(journal_dir, STAGE_UNDO_WINDOW, tx);
    let out = out.clone();
    thread::Builder::new()
        .name("dfdu-agent-del".into())
        .spawn(move || {
            for msg in rx {
                out.send(&Up::Deleter(msg));
            }
        })
        .expect("spawn deleter output");
    handle
}

/// `n` measurers (at least one, which fails every file where nothing can be measured), their
/// results sent as they come.
fn start_measurers<W: Write + Send + 'static>(
    out: &Arc<Out<W>>,
    n: usize,
) -> crossbeam_channel::Sender<MeasureReq> {
    let (tx, rx) = crossbeam_channel::unbounded::<Vec<MeasureMsg>>();
    let reqs = spawn_measurers(n, tx);
    let out = out.clone();
    thread::Builder::new()
        .name("dfdu-agent-measure".into())
        .spawn(move || {
            for msgs in rx {
                out.send(&Up::Measured(msgs));
            }
        })
        .expect("spawn measurer output");
    reqs
}

/// The rules in `files` built for this host, and the answer to [`Down::Rules`]: this host's
/// platform and the variables the rules read, their fingerprint, and what `root` classifies
/// against as a scan root.
fn build_rules(files: &[(Option<String>, String)], root: &[u8]) -> (Arc<RuleSet>, Up) {
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
    let answer = Up::Rules {
        platform: env.platform.as_str().into(),
        vars: env.used_by(&texts),
        fingerprint: set.fingerprint(),
        root: WireClass::of(&ClassOpts::for_root(set.clone(), &root)),
    };
    (set, answer)
}
