use std::sync::atomic::Ordering::Relaxed;

use crossbeam_channel::{Receiver, unbounded};
use dfdu_scan::{Limits, PortableBackend, Shared};
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
fn ask(query: Query) -> (Ask<PortableBackend>, Receiver<Command>) {
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
        scan: None,
    };
    (ask, rx)
}

/// The root is stat'ed on the journal's thread once it answered, and sent with the resume; a
/// root that is gone is sent as not stat'ed, and the resume is still counted, then applied.
#[test]
fn the_root_is_stat_ed_after_the_journal_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let scan = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
    let (mut a, rx) = ask(|_, _, _| Some(vec![]));
    a.path.clone_from(&root);
    a.scan = Some(scan.clone());
    let sent = a.sent.clone();
    a.run();
    let Ok(Command::Resume { root: Some(m), .. }) = rx.try_recv() else {
        panic!("a resume with the root's stat");
    };
    let now = Meta::from_metadata(&std::fs::metadata(&root).unwrap());
    assert_eq!((m.dev, m.ino, m.mtime), (now.dev, now.ino, now.mtime));

    let (mut a, rx) = ask(|_, _, _| Some(vec![]));
    a.path = root.join("gone");
    a.scan = Some(scan);
    a.run();
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Resume {
            changed: Some(_),
            root: None,
            ..
        })
    ));
    assert_eq!(sent.load(SeqCst), 1);
}

/// A loaded tree whose root is gone by the time it is resumed ends as a scan of that path
/// would: the root, with its error, and nothing below it.
#[test]
fn a_root_that_is_gone_is_listed_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("tree");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("f"), b"data").unwrap();
    let mut tree = loaded(&root);
    let r = tree.roots().next().unwrap();
    let f = tree.add_file(
        r,
        b"f",
        &Meta::from_metadata(&std::fs::metadata(root.join("f")).unwrap()),
        true,
        None,
    );
    let own = tree.own(f);
    tree.propagate_add(r, &own);
    std::fs::remove_dir_all(&root).unwrap();
    let e = engine();
    e.resume_with(tree, Some(AT), JobOptions::default(), |_, _, _| {
        Some(vec![])
    });
    assert!(e.wait_idle(Some(Duration::from_secs(10))).is_idle());
    let (items, error) = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (m.tree.total(r).items, m.tree.error(r))
    });
    assert_eq!(items, 1, "the root alone");
    assert!(error.is_some());
    e.shutdown(Duration::from_millis(200));
}

/// A query that panics sends the resume without changes, so everything is rescanned, and the
/// resume stays counted.
#[test]
fn a_panicking_query_resumes_everything() {
    let (a, rx) = ask(|_, _, _| panic!("injected journal panic"));
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
    let (a, rx) = ask(|_, _, _| Some(vec![]));
    let sent = a.sent.clone();
    a.cancel.store(true, SeqCst);
    a.run();
    assert!(rx.try_recv().is_err());
    assert_eq!(sent.load(SeqCst), 0);

    let (a, rx) = ask(|_, _, _| Some(vec![]));
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
    e.resume_with(loaded(&root), Some(AT), JobOptions::default(), |_, _, _| {
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
    e.resume_with(loaded(&root), Some(AT), JobOptions::default(), |_, _, _| {
        thread::sleep(Duration::from_secs(2));
        Some(vec![])
    });
    let t0 = Instant::now();
    e.shutdown(Duration::from_millis(100));
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
}
