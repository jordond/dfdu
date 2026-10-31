//! Loaded trees resumed with the directories a change journal would name, given by hand.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use dfdu_engine::{Engine, JournalPosition};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::Tree;

use super::portable::engine;

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

/// [`resume_with`] the default options.
pub fn resume(tree: Tree, changed: &[&str]) -> Engine<PortableBackend> {
    resume_with(tree, JobOptions::default(), changed)
}
