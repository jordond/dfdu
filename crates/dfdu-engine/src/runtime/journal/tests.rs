use std::sync::atomic::Ordering::Relaxed;

use crossbeam_channel::{Receiver, unbounded};
use dfdu_scan::PortableBackend;
use dfdu_tree::{DirState, Meta};

use super::*;
use crate::{EngineOptions, WaitIdle};

const AT: JournalPosition = JournalPosition {
    volume: [1; 16],
    event_id: 7,
    time: 0,
};

fn engine() -> Engine<PortableBackend> {
    Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            ..Default::default()
        },
    )
}

/// The tree of the directory `root` as loaded from a file that saw nothing in it: the root,
/// complete.
fn loaded(root: &Path) -> Tree {
    let mut t = Tree::new();
    let meta = Meta::from_metadata(&std::fs::metadata(root).unwrap());
    let r = t.new_root(root.as_os_str().as_encoded_bytes(), &meta, true);
    if let Some(d) = t.dir_mut(r) {
        d.state = DirState::Complete;
    }
    t
}

/// A query of `query` counted as sent, and where its resume goes.
fn ask(query: Query) -> (Ask, Receiver<Command>) {
    let (tx, rx) = unbounded();
    let ask = Ask {
        tx,
        sent: Arc::new(AtomicU64::new(1)),
        cancel: Arc::new(AtomicBool::new(false)),
        node: NodeRef::from_bits(0),
        opts: JobOptions::default(),
        path: PathBuf::from("/nowhere"),
        at: AT,
        query,
    };
    (ask, rx)
}

/// A query that panics sends the resume without changes, so everything is rescanned, and the
/// resume stays counted.
#[test]
fn a_panicking_query_resumes_everything() {
    let (a, rx) = ask(|_, _| panic!("injected journal panic"));
    let sent = a.sent.clone();
    a.run();
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Resume { changed: None, .. })
    ));
    assert_eq!(sent.load(SeqCst), 1);
}

/// A cancelled query sends nothing and is no longer counted as sent, nor is one whose engine
/// is gone.
#[test]
fn a_cancelled_or_unsent_resume_is_uncounted() {
    let (a, rx) = ask(|_, _| Some(vec![]));
    let sent = a.sent.clone();
    a.cancel.store(true, SeqCst);
    a.run();
    assert!(rx.try_recv().is_err());
    assert_eq!(sent.load(SeqCst), 0);

    let (a, rx) = ask(|_, _| Some(vec![]));
    let sent = a.sent.clone();
    drop(rx);
    a.run();
    assert_eq!(sent.load(SeqCst), 0);
}

/// Holds the late answer of the test below until set.
static ANSWER: AtomicBool = AtomicBool::new(false);

/// Waiting for idle waits for a resume whose journal answers late; once it answered, the
/// resume is applied and the engine is idle.
#[test]
fn idle_waits_for_a_late_journal_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    std::fs::write(root.join("new"), b"data").unwrap();
    let e = engine();
    e.resume_with(loaded(&root), Some(AT), JobOptions::default(), |_, _| {
        while !ANSWER.load(Relaxed) {
            thread::sleep(Duration::from_millis(1));
        }
        Some(vec![PathBuf::new()])
    });
    assert_eq!(
        e.wait_idle(Some(Duration::from_millis(100))),
        WaitIdle::TimedOut
    );
    assert_eq!(e.asking.lock().len(), 1);
    ANSWER.store(true, Relaxed);
    assert!(e.wait_idle(Some(Duration::from_secs(10))).is_idle());
    let items = e.read(|m| m.tree.roots().next().map(|r| m.tree.total(r).items));
    assert_eq!(items, Some(2), "the root and the file the resume listed");
    e.shutdown(Duration::from_millis(200));
}

/// Shutting down before the journal answered waits for it no longer than the bound.
#[test]
fn shutdown_does_not_wait_for_the_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let e = engine();
    e.resume_with(loaded(&root), Some(AT), JobOptions::default(), |_, _| {
        thread::sleep(Duration::from_secs(2));
        Some(vec![])
    });
    let t0 = Instant::now();
    e.shutdown(Duration::from_millis(100));
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}
