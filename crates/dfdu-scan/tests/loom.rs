//! `loom` tests of the scanner's hand-written synchronisation. They run the real `Shared`,
//! `Worker`, watchdog round and `Budget` on `loom`'s atomics and locks (`src/sync.rs`) with a
//! small in-memory backend, and check the messages the applier would receive.
//!
//! ```text
//! RUSTFLAGS="--cfg loom" CARGO_TARGET_DIR=target/loom cargo test --release -p dfdu-scan --test loom
//! ```
#![cfg(loom)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64 as StdAtomicU64, AtomicUsize as StdAtomicUsize, Ordering};

use dfdu_scan::sync::Sleepers;
use dfdu_scan::{
    Backend, Batch, Block, Budget, Clock, DirId, DirStream, JobId, JobOptions, Limits,
    ListingStatus, Raw, RawBlock, ScanMsg, Shared, Sink, StepResult, Worker,
};
use dfdu_sys::Storage;
use dfdu_tree::{Kind, Meta};
use loom::thread;

/// One entry of a directory: a file, or a subdirectory by index.
#[derive(Clone, Copy, Debug)]
enum E {
    F,
    D(usize),
    /// A file whose block fails to stat (recorded as a panicked chunk).
    Bad,
}

/// The device of a mount on a spinning disk: [`Mem::mount_storage`] says so.
const SPINNING: u64 = 2;

/// Directories by index, 0 being the root. `stuck` makes the first `open` of a directory
/// report "not ready" that many times, like a call that hangs and then returns. `devs` puts
/// directories on other devices (mounts); calls on [`SPINNING`] directories are counted in
/// `calls`.
struct Mem {
    dirs: Vec<Vec<E>>,
    stuck: HashMap<usize, StdAtomicUsize>,
    calls: Arc<Calls>,
}

/// Calls in progress on the spinning disk, and the most seen at once.
struct Calls {
    devs: Vec<u64>,
    active: loom::sync::atomic::AtomicUsize,
    max: StdAtomicUsize,
}

impl Calls {
    /// Runs `f` as a call on directory `d`.
    fn call<R>(&self, d: usize, f: impl FnOnce() -> R) -> R {
        if self.devs[d] != SPINNING {
            return f();
        }
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(n, Ordering::Relaxed);
        let r = f();
        self.active.fetch_sub(1, Ordering::SeqCst);
        r
    }
}

impl Mem {
    fn new(dirs: Vec<Vec<E>>) -> Self {
        let n = dirs.len();
        Mem {
            dirs,
            stuck: HashMap::new(),
            calls: Arc::new(Calls {
                devs: vec![1; n],
                active: loom::sync::atomic::AtomicUsize::new(0),
                max: StdAtomicUsize::new(0),
            }),
        }
    }
    fn stuck(mut self, dir: usize, times: usize) -> Self {
        self.stuck.insert(dir, StdAtomicUsize::new(times));
        self
    }
    fn devs(mut self, devs: &[u64]) -> Self {
        Arc::get_mut(&mut self.calls).expect("not shared yet").devs = devs.to_vec();
        self
    }
}

fn meta(kind: Kind, ino: u64, dev: u64) -> Meta {
    Meta {
        kind,
        apparent: 1,
        alloc: 1,
        mtime: 0,
        dev,
        ino,
        nlink: 1,
        mount: 0,
    }
}

struct MemStream {
    dir: usize,
    entries: Vec<E>,
    pos: usize,
    calls: Arc<Calls>,
}

struct MemRaw {
    dir: usize,
    entries: Vec<E>,
    calls: Arc<Calls>,
}

impl RawBlock<usize> for MemRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }
    fn stat(&mut self, out: &mut Block<usize>) -> io::Result<()> {
        let calls = self.calls.clone();
        calls.call(self.dir, || {
            if self.entries.iter().any(|e| matches!(e, E::Bad)) {
                return Err(io::Error::other("stat failed"));
            }
            let dev = self.calls.devs[self.dir];
            for (i, e) in self.entries.drain(..).enumerate() {
                match e {
                    E::D(d) => out.push(
                        format!("d{d}").as_bytes(),
                        Ok(meta(Kind::Dir, d as u64, self.calls.devs[d])),
                        Some(d),
                    ),
                    _ => out.push(
                        format!("f{i}").as_bytes(),
                        Ok(meta(Kind::File, 1000 + i as u64, dev)),
                        None,
                    ),
                }
            }
            Ok(())
        })
    }
}

impl DirStream for MemStream {
    type Loc = usize;
    fn next_raw(&mut self, max: usize, _: usize) -> io::Result<(Raw<usize>, bool)> {
        let calls = self.calls.clone();
        calls.call(self.dir, || {
            let end = (self.pos + max).min(self.entries.len());
            let raw = MemRaw {
                dir: self.dir,
                entries: self.entries[self.pos..end].to_vec(),
                calls: self.calls.clone(),
            };
            self.pos = end;
            Ok((Box::new(raw) as Raw<usize>, self.pos < self.entries.len()))
        })
    }
}

impl Backend for Mem {
    type Loc = usize;
    type Stream = MemStream;
    fn loc_from_path(&self, _: &Path) -> usize {
        0
    }
    fn stat_root(&self, _: &usize) -> io::Result<Meta> {
        Ok(meta(Kind::Dir, 0, self.calls.devs[0]))
    }
    fn open(&self, d: &usize, _storage: Storage) -> io::Result<MemStream> {
        self.calls.call(*d, || {
            if let Some(n) = self.stuck.get(d)
                && n.load(Ordering::Relaxed) > 0
            {
                n.fetch_sub(1, Ordering::Relaxed);
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Ok(MemStream {
                dir: *d,
                entries: self.dirs[*d].clone(),
                pos: 0,
                calls: self.calls.clone(),
            })
        })
    }
    fn mount_storage(&self, _mount: u64, dev: u64) -> Storage {
        if dev == SPINNING {
            Storage::Rotational
        } else {
            Storage::Unknown
        }
    }
}

/// The applier's inbox as one ordered log.
#[derive(Default)]
struct Log {
    msgs: loom::sync::Mutex<Vec<ScanMsg>>,
}

impl Log {
    fn take(&self) -> Vec<ScanMsg> {
        std::mem::take(&mut *self.msgs.lock().unwrap())
    }
    fn closed(&self, job: JobId, dir: DirId) -> bool {
        self.msgs
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, ScanMsg::Close { job: j, dir: d, .. } if *j == job && *d == dir))
    }
}

impl Sink for Log {
    fn try_send(&self, batch: Batch) -> Result<(), Batch> {
        self.msgs.lock().unwrap().extend(batch.msgs);
        Ok(())
    }
    fn send(&self, batch: Batch) {
        self.msgs.lock().unwrap().extend(batch.msgs);
    }
    fn send_control(&self, msg: ScanMsg) {
        self.msgs.lock().unwrap().push(msg);
    }
}

/// A clock for one thread: the first `old` readings are 1 (a call started long ago), later
/// ones `NOW`. Used by one thread only, so a plain atomic is enough.
struct Clk {
    old: StdAtomicU64,
}

const NOW: u64 = 1_000;
const STALL_NS: u64 = 100;

impl Clk {
    fn new(old: u64) -> Self {
        Clk {
            old: StdAtomicU64::new(old),
        }
    }
}

impl Clock for Clk {
    fn now(&self) -> u64 {
        if self.old.load(Ordering::Relaxed) > 0 {
            self.old.fetch_sub(1, Ordering::Relaxed);
            1
        } else {
            NOW
        }
    }
}

const JOB: JobId = JobId(1);

fn limits(block: usize, credit: usize, outbox_msgs: usize) -> Limits {
    Limits {
        block_entries: block,
        block_name_bytes: 1 << 20,
        outbox_bytes: 1 << 30,
        outbox_msgs,
        flush_age_ns: 0,
        stall_ns: STALL_NS,
        chunk_credit: credit,
        ..Limits::default()
    }
}

fn shared(mem: Mem, limits: Limits) -> Arc<Shared<Mem>> {
    let sh = Arc::new(Shared::new(mem, limits));
    sh.start_job(JOB, Path::new("/"), JobOptions::default());
    sh
}

/// Steps `w` until it has nothing to do. A call that is not ready lets other threads run, as
/// the real runtime sleeps then.
fn run(w: &mut Worker<Mem>, sh: &Shared<Mem>, sink: &Log, clock: &dyn Clock) {
    loop {
        match w.step(sh, sink, clock) {
            StepResult::Idle => return,
            StepResult::Blocked | StepResult::WaitingBudget => thread::yield_now(),
            StepResult::Progress => {}
        }
    }
}

/// Runs every worker until none has work, flushes them all, then one watchdog round.
fn drain(ws: &mut [Worker<Mem>], sh: &Shared<Mem>, sink: &Log) {
    let clock = Clk::new(0);
    loop {
        let mut progress = false;
        for w in ws.iter_mut() {
            while w.step(sh, sink, &clock) != StepResult::Idle {
                progress = true;
            }
            w.flush(sh, sink);
        }
        if !progress {
            break;
        }
    }
    sh.watchdog_round(sink, NOW);
}

#[derive(Debug, Default)]
struct Seen {
    closes: HashMap<DirId, (u32, ListingStatus)>,
    chunks: HashMap<DirId, u32>,
    stalled: Vec<DirId>,
    quiescent: usize,
}

/// [`check_order`] on the whole inbox, plus: every named directory has closed, with the
/// number of chunks it sent.
fn check(log: &[ScanMsg], unclosed_ok: &[DirId]) -> Seen {
    let (s, named) = check_order(log, unclosed_ok);
    for d in &named {
        let Some((n, _)) = s.closes.get(d) else {
            panic!("{d:?} never closed: {log:?}");
        };
        assert_eq!(
            *n,
            s.chunks.get(d).copied().unwrap_or(0),
            "chunk count of {d:?}: {log:?}"
        );
    }
    s
}

/// Checks the inbox order the applier relies on: every message about a directory comes after
/// the chunk naming it; at most one `Close` per directory; at a `Quiescent`, every named
/// directory not in `unclosed_ok` is closed. Returns what it saw and the named directories.
fn check_order(log: &[ScanMsg], unclosed_ok: &[DirId]) -> (Seen, HashSet<DirId>) {
    let mut named: HashSet<DirId> = [DirId::ROOT].into();
    let mut s = Seen::default();
    for (i, m) in log.iter().enumerate() {
        match m {
            ScanMsg::Chunk(c) => {
                assert!(
                    named.contains(&c.dir),
                    "chunk for unnamed {:?} at {i}: {log:?}",
                    c.dir
                );
                *s.chunks.entry(c.dir).or_default() += 1;
                for k in 0..u32::try_from(c.subdirs.len()).unwrap() {
                    named.insert(DirId(c.first_subdir.0 + k));
                }
            }
            ScanMsg::Close {
                dir,
                chunks,
                status,
                ..
            } => {
                assert!(named.contains(dir), "close for unnamed {dir:?}: {log:?}");
                let prev = s.closes.insert(*dir, (*chunks, status.clone()));
                assert!(prev.is_none(), "second close for {dir:?}: {log:?}");
            }
            ScanMsg::Stalled { dir, .. } => {
                assert!(named.contains(dir), "stall for unnamed {dir:?}: {log:?}");
                s.stalled.push(*dir);
            }
            ScanMsg::Quiescent { .. } => {
                s.quiescent += 1;
                for d in &named {
                    assert!(
                        s.closes.contains_key(d) || unclosed_ok.contains(d),
                        "quiescent with {d:?} open at {i}: {log:?}"
                    );
                }
            }
            // A resume's check names directories by batch index, not by `DirId`.
            ScanMsg::Verified { .. } => {}
        }
    }
    (s, named)
}

/// Explores `f` with at most `bound` preemptions per execution, or every interleaving with
/// `None` (`LOOM_MAX_PREEMPTIONS` overrides it), and prints how many executions that took. The
/// bounds keep the suite to seconds and are large enough for each test to catch the races it
/// targets when the code under test is broken on purpose.
fn model(name: &str, bound: Option<usize>, f: impl Fn() + Sync + Send + 'static) {
    let runs = Arc::new(StdAtomicUsize::new(0));
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = bound;
    }
    b.max_branches = 100_000;
    let start = std::time::Instant::now();
    let r = runs.clone();
    b.check(move || {
        r.fetch_add(1, Ordering::Relaxed);
        f();
    });
    eprintln!(
        "{name}: {} executions, preemption bound {:?}, {:.1?}",
        runs.load(Ordering::Relaxed),
        b.preemption_bound,
        start.elapsed()
    );
}

/// Outbox flush and publication, unfinished-part counting and `Close` election, `outstanding`
/// and quiescence. Two workers and the watchdog scan a root with one subdirectory of three
/// one-entry blocks, two of them stat'ed by chunk tasks (one fails). Whatever the order: each
/// directory closes once, after the chunk naming it, with all its chunks counted and the failed
/// block in its status; no `Quiescent` while anything is open; quiescence at the end.
#[test]
fn flush_close_election_and_quiescence() {
    model("flush_close_election_and_quiescence", Some(2), || {
        let mem = Mem::new(vec![vec![E::D(1)], vec![E::F, E::Bad, E::F]]);
        let sh = shared(mem, limits(1, 2, 1));
        let log = Arc::new(Log::default());
        let mut ws: Vec<_> = (0..2).map(|_| Worker::new(&sh, true)).collect();
        let hs: Vec<_> = ws
            .drain(..)
            .map(|mut w| {
                let (sh, log) = (sh.clone(), log.clone());
                thread::spawn(move || {
                    run(&mut w, &sh, &log, &Clk::new(0));
                    w
                })
            })
            .collect();
        let (sh2, log2) = (sh.clone(), log.clone());
        let wd = thread::spawn(move || {
            sh2.watchdog_round(&*log2, NOW);
        });
        let mut ws: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        wd.join().unwrap();
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[]);
        assert!(s.quiescent >= 1, "never quiescent: {msgs:?}");
        assert_eq!(s.closes.len(), 2);
        assert_eq!(
            s.closes[&DirId(1)],
            (
                3,
                ListingStatus::Partial {
                    panicked: 1,
                    cancelled: 0,
                    read_error: None
                }
            )
        );
        assert_eq!(sh.outstanding(), (0, 0));
    });
}

/// Lists the root on this thread with `w` until its listing has ended, leaving the root's
/// messages and its child tasks in `w`'s outbox, unflushed (outbox limits permitting).
fn list_root(w: &mut Worker<Mem>, sh: &Shared<Mem>, log: &Log) {
    let clock = Clk::new(0);
    w.step(sh, log, &clock);
    while w.is_running() {
        w.step(sh, log, &clock);
    }
}

/// Stall handoff. Worker A holds the root's chunk and both child tasks in its outbox and gets
/// stuck opening the first child. The watchdog flushes A's outbox, which publishes the second
/// child, and appends `Stalled`; worker B can take the second child. Each child is listed by
/// exactly one worker (one `Close` each), and `Stalled` follows the chunk naming the directory
/// and names only the directory whose call was stuck.
#[test]
fn stall_report_and_handoff() {
    model("stall_report_and_handoff", Some(4), || {
        let mem = Mem::new(vec![vec![E::D(1), E::D(2)], vec![E::F], vec![E::F]]).stuck(1, 1);
        let sh = shared(mem, limits(8, 0, 1000));
        let log = Arc::new(Log::default());
        let mut a = Worker::new(&sh, true);
        let mut b = Worker::new(&sh, true);
        list_root(&mut a, &sh, &log);
        assert_eq!(a.slot.outbox.lock().unpublished(), 2);
        let (sh1, log1) = (sh.clone(), log.clone());
        // A's first call (opening child 1) started long ago; its later ones are recent.
        let ta = thread::spawn(move || {
            run(&mut a, &sh1, &log1, &Clk::new(1));
            a
        });
        let (sh2, log2) = (sh.clone(), log.clone());
        let tb = thread::spawn(move || {
            run(&mut b, &sh2, &log2, &Clk::new(0));
            b
        });
        sh.watchdog_round(&*log, NOW);
        let mut ws = vec![ta.join().unwrap(), tb.join().unwrap()];
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[]);
        assert!(s.stalled.len() <= 1, "{msgs:?}");
        assert!(
            s.stalled.iter().all(|d| *d == DirId(1)),
            "spurious stall: {msgs:?}"
        );
        assert!(s.quiescent >= 1);
        assert_eq!(sh.outstanding(), (0, 0));
    });
}

/// Abandon racing the stuck part's return: the applier abandons child 1 while the worker stuck
/// opening it returns and finishes it, then runs a watchdog round. The part is accounted to
/// exactly one side: recorded as abandoned while running and released by its own flush, or
/// finished before the applier looked. Child 1 closes once, and `outstanding` and
/// `outstanding_abandoned` both end at zero.
#[test]
fn abandon_races_stuck_part_returning() {
    model("abandon_races_stuck_part_returning", Some(5), || {
        let mem = Mem::new(vec![vec![E::D(1)], vec![E::F]]).stuck(1, 1);
        let sh = shared(mem, limits(8, 0, 1000));
        let log = Arc::new(Log::default());
        let mut a = Worker::new(&sh, true);
        list_root(&mut a, &sh, &log);
        let (sh1, log1) = (sh.clone(), log.clone());
        let ta = thread::spawn(move || {
            run(&mut a, &sh1, &log1, &Clk::new(1));
            a
        });
        while !sh.abandon(JOB, DirId(1)) {
            thread::yield_now();
        }
        sh.watchdog_round(&*log, NOW);
        let mut ws = vec![ta.join().unwrap()];
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[DirId(1)]);
        assert!(s.quiescent >= 1);
        assert_eq!(sh.outstanding(), (0, 0), "{msgs:?}");
    });
}

/// `outstanding` and quiescence with a part that stays stuck: while worker A is stuck in child
/// 1 (or about to take it), the applier abandons the directory and the watchdog runs. Once the
/// abandon is recorded, a watchdog round reports `Quiescent` although child 1 has no `Close`;
/// when the stuck call returns, its flush releases the abandoned count and nothing is reported
/// twice.
#[test]
fn quiescence_with_abandoned_stuck_part() {
    model("quiescence_with_abandoned_stuck_part", Some(6), || {
        let mem = Mem::new(vec![vec![E::D(1)], vec![E::F]]).stuck(1, 1);
        let sh = shared(mem, limits(8, 0, 1000));
        let log = Arc::new(Log::default());
        let mut a = Worker::new(&sh, true);
        list_root(&mut a, &sh, &log);
        a.flush(&sh, &*log);
        let (sh1, log1) = (sh.clone(), log.clone());
        // A takes child 1 from the queue and stops in the stuck call, or closes it cancelled.
        let ta = thread::spawn(move || {
            let clock = Clk::new(1);
            while !matches!(
                a.step(&sh1, &*log1, &clock),
                StepResult::Blocked | StepResult::Idle
            ) {}
            a
        });
        while !sh.abandon(JOB, DirId(1)) {
            thread::yield_now();
        }
        sh.watchdog_round(&*log, NOW);
        let mut a = ta.join().unwrap();
        sh.watchdog_round(&*log, NOW);
        let (early, _) = check_order(&log.msgs.lock().unwrap(), &[DirId(1)]);
        assert!(
            early.quiescent >= 1,
            "not quiescent with the stuck part abandoned"
        );
        run(&mut a, &sh, &log, &Clk::new(0));
        let mut ws = vec![a];
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[DirId(1)]);
        assert_eq!(s.quiescent, early.quiescent, "{msgs:?}");
        assert_eq!(sh.outstanding(), (0, 0), "{msgs:?}");
    });
}

/// Early-stop bitset: the applier stops child 1 while worker A takes its task. If the stop was
/// set before A began, A closes the directory `Cancelled` without reading it; either way it
/// closes once.
#[test]
fn early_stop_seen_by_later_take() {
    model("early_stop_seen_by_later_take", None, || {
        let mem = Mem::new(vec![vec![E::D(1)], vec![E::F]]);
        let sh = shared(mem, limits(8, 0, 1000));
        let log = Arc::new(Log::default());
        let mut a = Worker::new(&sh, true);
        list_root(&mut a, &sh, &log);
        a.flush(&sh, &*log);
        let stopped = Arc::new(loom::sync::atomic::AtomicBool::new(false));
        let (sh1, st1) = (sh.clone(), stopped.clone());
        let tu = thread::spawn(move || {
            sh1.stop_dirs(JOB, [DirId(1)]);
            st1.store(true, Ordering::SeqCst);
        });
        let after_stop = stopped.load(Ordering::SeqCst);
        run(&mut a, &sh, &log, &Clk::new(0));
        tu.join().unwrap();
        drain(&mut [a], &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[]);
        if after_stop {
            assert_eq!(
                s.closes[&DirId(1)],
                (0, ListingStatus::Cancelled),
                "{msgs:?}"
            );
        }
    });
}

/// Byte budget: two producers take 2 of 3 bytes each, one blocking and one trying first; the
/// consumer releases what each sent. Use never exceeds the capacity, a blocked producer is
/// woken by the release (a lost wake would deadlock), the budget ends empty, and a request
/// larger than the capacity is clamped to it.
#[test]
fn budget_bounds_and_wakes() {
    model("budget_bounds_and_wakes", None, || {
        let b = Arc::new(Budget::new(3));
        let (tx, rx) = loom::sync::mpsc::channel();
        let producers: Vec<_> = (0..2)
            .map(|i| {
                let (b, tx) = (b.clone(), tx.clone());
                thread::spawn(move || {
                    let n = if i == 0 {
                        b.acquire(2)
                    } else {
                        b.try_acquire(2).unwrap_or_else(|| b.acquire(2))
                    };
                    assert!(b.used() <= b.capacity());
                    tx.send(n).unwrap();
                })
            })
            .collect();
        for _ in 0..2 {
            let n = rx.recv().unwrap();
            assert!(b.used() <= b.capacity());
            b.release(n);
        }
        for p in producers {
            p.join().unwrap();
        }
        assert_eq!(b.used(), 0);
        assert_eq!(b.acquire(10), 3);
        assert_eq!(b.try_acquire(1), None);
        b.release(3);
        assert_eq!(b.used(), 0);
    });
}

/// Focus atomic: the root is the focus, and the UI moves the focus elsewhere while a worker
/// names the root's child. The child is published after the change, so it must go to the normal
/// queue: it was named either before the change (with the old epoch, which no longer matches)
/// or after it (the root is no longer the focus).
#[test]
fn focus_change_does_not_prioritise_old_focus() {
    model("focus_change_does_not_prioritise_old_focus", None, || {
        let mem = Mem::new(vec![vec![E::D(1)], vec![]]);
        let sh = shared(mem, limits(8, 0, 1000));
        sh.set_focus(JOB, DirId::ROOT);
        let log = Arc::new(Log::default());
        let mut w = Worker::new(&sh, true);
        let clock = Clk::new(0);
        // Take the root task and open it; the next step reads and names the child.
        w.step(&sh, &*log, &clock);
        w.step(&sh, &*log, &clock);
        let sh1 = sh.clone();
        let ui = thread::spawn(move || sh1.set_focus(JOB, DirId(99)));
        w.step(&sh, &*log, &clock);
        ui.join().unwrap();
        assert_eq!(w.slot.outbox.lock().unpublished(), 1);
        w.flush(&sh, &*log);
        assert_eq!(sh.queued(), (0, 1), "child of the old focus prioritised");
    });
}

/// Wake of idle workers (the runtime's `Sleepers`): a worker that found no task registers,
/// looks at the queues again and parks; publishing a task wakes every registered worker.
/// A publication racing the worker's decision to sleep is never missed: under `loom` a park
/// has no timeout, so a missed wake is a deadlock.
#[test]
fn idle_worker_wake_is_not_lost() {
    model("idle_worker_wake_is_not_lost", None, || {
        let sh = Arc::new(Shared::new(Mem::new(vec![vec![]]), limits(8, 0, 1)));
        let sleepers = Arc::new(Sleepers::default());
        let s = sleepers.clone();
        sh.set_waker(Box::new(move || s.wake_all()));
        let log = Arc::new(Log::default());
        let mut w = Worker::new(&sh, true);
        let (sh1, log1) = (sh.clone(), log.clone());
        let t = thread::spawn(move || {
            let clock = Clk::new(0);
            loop {
                if w.step(&sh1, &*log1, &clock) == StepResult::Idle {
                    if log1.closed(JOB, DirId::ROOT) {
                        break;
                    }
                    sleepers.sleep(|| sh1.has_queued(), std::time::Duration::from_millis(5));
                }
            }
        });
        sh.start_job(JOB, Path::new("/"), JobOptions::default());
        t.join().unwrap();
    });
}

/// Directories 1-3 on the spinning disk: `d1` is a mount root.
const DEVS: [u64; 4] = [1, SPINNING, SPINNING, SPINNING];

/// A scan entering the spinning-disk mount `d1`, limited to one part at a time.
fn spinning_mount(mem: Mem, outbox_msgs: usize) -> Arc<Shared<Mem>> {
    let sh = Arc::new(Shared::new(
        mem,
        Limits {
            rotational_parts: 1,
            ..limits(8, 0, outbox_msgs)
        },
    ));
    let opts = JobOptions {
        cross_mounts: true,
        ..JobOptions::default()
    };
    sh.start_job(JOB, Path::new("/"), opts);
    sh
}

/// Mount limit: two workers and the watchdog scan a root whose subdirectory `d1` is a mount on
/// a spinning disk limited to one part, with two subdirectories of its own. A worker that takes
/// a task of the mount while the other holds its permit leaves it waiting in the gate; the part
/// that ends hands its permit to it. Calls on the mount never overlap, every directory closes
/// once, and the gate ends with no permit held and nothing waiting.
#[test]
fn mount_limit_waits_and_hands_over() {
    model("mount_limit_waits_and_hands_over", Some(3), || {
        let mem = Mem::new(vec![
            vec![E::D(1)],
            vec![E::D(2), E::D(3)],
            vec![E::F],
            vec![E::F],
        ])
        .devs(&DEVS);
        let calls = mem.calls.clone();
        let sh = spinning_mount(mem, 1);
        let log = Arc::new(Log::default());
        let mut ws: Vec<_> = (0..2).map(|_| Worker::new(&sh, true)).collect();
        let hs: Vec<_> = ws
            .drain(..)
            .map(|mut w| {
                let (sh, log) = (sh.clone(), log.clone());
                thread::spawn(move || {
                    run(&mut w, &sh, &log, &Clk::new(0));
                    w
                })
            })
            .collect();
        sh.watchdog_round(&*log, NOW);
        let mut ws: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[]);
        assert_eq!(s.closes.len(), 4, "{msgs:?}");
        assert!(s.quiescent >= 1);
        assert_eq!(sh.outstanding(), (0, 0));
        assert!(calls.max.load(Ordering::Relaxed) <= 1, "calls overlapped");
        let g = &sh.gate_stats()[0];
        assert_eq!((g.limit, g.running, g.pending), (1, 0, 0), "{g:?}");
    });
}

/// A stuck part gives its permit back. Worker A holds the permit of a mount limited to one part
/// and is stuck opening `d2`; `d3` waits in the gate, taken by worker B. Then the watchdog, A
/// returning from its call and B race: the watchdog takes A's permit back and requeues `d3`, or
/// A ends `d2` first and hands its permit to `d3`. Either way `d3` runs once, each directory
/// closes once, and the gate ends with no permit held and nothing waiting.
#[test]
fn stuck_part_gives_back_its_permit() {
    model("stuck_part_gives_back_its_permit", Some(3), || {
        let mem = Mem::new(vec![
            vec![E::D(1)],
            vec![E::D(2), E::D(3)],
            vec![E::F],
            vec![E::F],
        ])
        .devs(&DEVS)
        .stuck(2, 1);
        let sh = spinning_mount(mem, 1000);
        let log = Arc::new(Log::default());
        let mut a = Worker::new(&sh, true);
        let mut b = Worker::new(&sh, true);
        // A lists the root and `d1` with large outbox limits, so `d2` and `d3` stay on its own
        // stack, then takes `d2` and stops in the stuck call; every reading of its clock is old.
        let old = Clk::new(u64::MAX);
        while a.step(&sh, &*log, &old) != StepResult::Blocked {}
        a.flush(&sh, &*log);
        // B takes `d3` from the queue and leaves it waiting for A's permit.
        assert_eq!(b.step(&sh, &*log, &Clk::new(0)), StepResult::Idle);
        assert!(!b.is_running());
        assert_eq!(sh.gate_stats()[0].pending, 1);
        let (sh1, log1) = (sh.clone(), log.clone());
        let ta = thread::spawn(move || {
            run(&mut a, &sh1, &log1, &Clk::new(u64::MAX));
            a
        });
        let (sh2, log2) = (sh.clone(), log.clone());
        let tb = thread::spawn(move || {
            run(&mut b, &sh2, &log2, &Clk::new(0));
            b
        });
        sh.watchdog_round(&*log, NOW);
        let mut ws = vec![ta.join().unwrap(), tb.join().unwrap()];
        drain(&mut ws, &sh, &log);
        let msgs = log.take();
        let s = check(&msgs, &[]);
        assert_eq!(s.closes.len(), 4, "{msgs:?}");
        assert_eq!(sh.outstanding(), (0, 0));
        let g = &sh.gate_stats()[0];
        assert_eq!((g.running, g.pending), (0, 0), "{g:?}");
    });
}
