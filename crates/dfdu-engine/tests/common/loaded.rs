//! Trees on disk loaded as a snapshot of them would be, and resumed with the directories a
//! change journal would name, given by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, JournalPosition};
use dfdu_scan::{Backend, JobOptions, PortableBackend};
use dfdu_tree::{DirState, Ix, Meta, Totals, Tree};

pub fn engine() -> Engine<PortableBackend> {
    Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            check_invariants: true,
            ..Default::default()
        },
    )
}

pub fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; bytes]).unwrap();
}

/// The tree of `root` as a snapshot of it now would load: every directory complete, a hard
/// link charged where it is first met.
pub fn loaded(root: &Path) -> Tree {
    fn add(t: &mut Tree, dir: Ix, path: &Path, seen: &mut Vec<(u64, u64)>) {
        let mut entries: Vec<_> = fs::read_dir(path).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for e in entries {
            let file_name = e.file_name();
            let name = dfdu_tree::name_bytes(&file_name);
            let meta = PortableBackend::default().stat_dir(&e.path()).unwrap();
            if e.file_type().unwrap().is_dir() {
                let d = t.add_dir(dir, &name, &meta);
                let own = t.own(d);
                t.propagate_add(dir, &own);
                add(t, d, &e.path(), seen);
                close(t, d);
            } else {
                let key = (meta.dev, meta.ino);
                let charged = !meta.is_hardlink() || !seen.contains(&key);
                seen.push(key);
                let f = t.add_file(dir, &name, &meta, charged, None);
                let own = t.own(f);
                t.propagate_add(dir, &own);
            }
        }
    }
    fn close(t: &mut Tree, d: Ix) {
        let r = t.dir_mut(d).unwrap();
        r.state = DirState::Complete;
        r.outcome = r.state;
        r.pending = 0;
        r.listing_closed = true;
        r.reported = true;
    }
    let mut t = Tree::default();
    let meta = PortableBackend::default().stat_dir(root).unwrap();
    let r = t.new_root(&dfdu_tree::name_bytes(root.as_os_str()), &meta, true);
    add(&mut t, r, root, &mut Vec::new());
    close(&mut t, r);
    t
}

/// What stat'ing `root` gives, as a resume is sent it after the journal was asked.
pub fn stat(root: &Path) -> Option<Box<Meta>> {
    PortableBackend::default()
        .stat_root(&root.to_path_buf())
        .ok()
        .map(Box::new)
}

/// The node at `rel` below the root, if there is one.
pub fn find(t: &Tree, rel: &str) -> Option<Ix> {
    let mut at = t.roots().next().unwrap();
    for n in rel.split('/').filter(|n| !n.is_empty()) {
        at = t.children(at).find(|&c| t.name(c) == n.as_bytes())?;
    }
    Some(at)
}

pub fn sizes(t: Totals) -> (u64, u64, u64) {
    (t.apparent, t.alloc, t.items)
}

/// The totals of a fresh scan of `root`.
pub fn scanned(root: &Path) -> (u64, u64, u64) {
    let e = engine();
    e.send(Command::Scan {
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let t = e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())));
    e.shutdown(Duration::from_secs(1));
    t
}

/// The root's totals.
pub fn totals(e: &Engine<PortableBackend>) -> (u64, u64, u64) {
    e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())))
}

/// Loads `tree` and resumes it with `opts` and `changed`, waiting until it is done.
pub fn resume_with(tree: Tree, opts: JobOptions, changed: &[&str]) -> Engine<PortableBackend> {
    resume_since(tree, opts, changed, 0)
}

/// [`resume_with`] a journal position read at `since` (seconds since the epoch).
pub fn resume_since(
    tree: Tree,
    opts: JobOptions,
    changed: &[&str],
    since: u64,
) -> Engine<PortableBackend> {
    let e = engine();
    let changed = changed.iter().map(PathBuf::from).collect();
    resume_changed(&e, tree, opts, changed, since);
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    e
}

/// What the change journal answers for the roots resumed here, by root ([`answer`]).
static JOURNAL: Mutex<Vec<(PathBuf, Vec<PathBuf>)>> = Mutex::new(Vec::new());

/// Resumes `tree` on `e` with `opts` ([`Engine::resume_with`]), the journal, its position read
/// at `since` (seconds since the epoch), naming `changed`. Returns once the tree is loaded.
pub fn resume_changed(
    e: &Engine<PortableBackend>,
    tree: Tree,
    opts: JobOptions,
    changed: Vec<PathBuf>,
    since: u64,
) {
    answer(&tree.path_buf(tree.roots().next().unwrap()), changed);
    resume_held(e, tree, opts, since);
}

/// [`resume_changed`], the journal answering only once [`answer`] gives it what changed below
/// the tree's root: until then the tree is as loaded.
pub fn resume_held(e: &Engine<PortableBackend>, tree: Tree, opts: JobOptions, since: u64) {
    let at = JournalPosition {
        volume: [0; 16],
        event_id: 0,
        time: since,
    };
    e.resume_with(tree, Some(at), opts, |root, _, cancel| {
        while !cancel.load(SeqCst) {
            let mut journal = JOURNAL.lock().unwrap();
            if let Some(i) = journal.iter().position(|(r, _)| r == root) {
                return Some(journal.swap_remove(i).1);
            }
            drop(journal);
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    });
}

/// What the journal answers for the resume of `root`: `changed`.
pub fn answer(root: &Path, changed: Vec<PathBuf>) {
    JOURNAL.lock().unwrap().push((root.to_path_buf(), changed));
}

/// Sets the mtime of `dir` and of every directory below it to a time long past, so that any
/// change in one moves it.
#[cfg(unix)]
pub fn age(dir: &Path) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            age(&e.path());
        }
    }
    set_mtime(dir, OLD);
}

/// Seconds since the epoch that [`age`] sets.
#[cfg(unix)]
pub const OLD: u64 = 1_000_000_000;

/// Sets the mtime of directory `dir`.
#[cfg(unix)]
pub fn set_mtime(dir: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::open(dir).unwrap().set_modified(t).unwrap();
}

/// [`resume_with`] the default options.
pub fn resume(tree: Tree, changed: &[&str]) -> Engine<PortableBackend> {
    resume_with(tree, JobOptions::default(), changed)
}
