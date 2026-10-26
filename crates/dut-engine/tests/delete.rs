//! Deletion against a real filesystem: verification against the scan, refusals, journal. Each
//! test runs with the portable backend, and on Windows with the Windows backend too, whose
//! file ids and volume serial numbers the deleter checks against.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dut_engine::{Command, DeleteSummary, Engine, EngineOptions, Mode};
use dut_scan::{Backend, JobOptions, PortableBackend};
use dut_tree::NodeRef;
use tempfile::TempDir;

/// A temporary directory under the canonical temp dir: the engine canonicalises scan roots
/// (on macOS the temp dir is behind the /var symlink), and paths compared with the tree's must
/// match.
fn tempdir(tag: &str) -> TempDir {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::Builder::new()
        .prefix(&format!("dut-del-{tag}-"))
        .tempdir_in(base)
        .unwrap()
}

fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; bytes]).unwrap();
}

fn fixture() -> TempDir {
    let dir = tempdir("tree");
    let root = dir.path();
    write(&root.join("keep/a.bin"), 10_000);
    write(&root.join("junk/b.bin"), 50_000);
    write(&root.join("junk/sub/c.bin"), 20_000);
    write(&root.join("file.bin"), 3_000);
    dir
}

fn engine<B: Backend>(backend: B, journal: Option<PathBuf>, read_only: bool) -> Engine<B> {
    Engine::start(
        backend,
        EngineOptions {
            journal_dir: journal,
            read_only,
            check_invariants: true,
            workers: 2,
            ..Default::default()
        },
    )
}

fn scan<B: Backend>(e: &Engine<B>, root: &Path) {
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
}

fn node<B: Backend>(e: &Engine<B>, rel: &str) -> NodeRef {
    e.read(|m| {
        let mut ix = m.tree.roots().next().unwrap();
        for part in rel.split('/') {
            ix = m
                .tree
                .children(ix)
                .find(|&c| m.tree.name(c) == part.as_bytes())
                .unwrap_or_else(|| panic!("{rel}"));
        }
        m.tree.node_ref(ix)
    })
}

fn delete<B: Backend>(e: &Engine<B>, items: Vec<NodeRef>, mode: Mode) -> DeleteSummary {
    e.send(Command::Delete { items, mode });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    e.read(|m| m.status.last_delete.clone())
        .expect("a plan finished")
}

fn root_alloc<B: Backend>(e: &Engine<B>) -> u64 {
    e.read(|m| m.tree.total(m.tree.roots().next().unwrap()).alloc)
}

fn journal_kinds(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    for f in fs::read_dir(dir).unwrap() {
        for line in fs::read_to_string(f.unwrap().path()).unwrap().lines() {
            out.push(
                dut_clean::check_line(line)
                    .expect("valid journal line")
                    .to_string(),
            );
        }
    }
    out
}

#[test]
fn permanent_delete_updates_tree_and_journal() {
    permanent_delete(PortableBackend::default);
    #[cfg(windows)]
    permanent_delete(dut_scan::WindowsBackend::default);
}

fn permanent_delete<B: Backend>(backend: fn() -> B) {
    let tree = fixture();
    let root = tree.path();
    let jdir = tempdir("journal");
    let e = engine(backend(), Some(jdir.path().to_path_buf()), false);
    scan(&e, root);
    let before = root_alloc(&e);
    let junk_bytes = e.read(|m| {
        m.tree
            .total(m.tree.resolve(node(&e, "junk")).unwrap())
            .alloc
    });
    let s = delete(&e, vec![node(&e, "junk")], Mode::Permanent);
    assert_eq!((s.deleted, s.failed, s.stale, s.refused), (1, 0, 0, 0));
    assert!(!root.join("junk").exists());
    assert!(root.join("keep/a.bin").exists());
    assert_eq!(root_alloc(&e), before - junk_bytes);
    e.read(|m| {
        assert!(
            m.tree
                .children(m.tree.roots().next().unwrap())
                .all(|c| m.tree.name(c) != b"junk")
        )
    });
    // Read the journal after shutdown: the deleter closes it then (Windows locks are mandatory).
    e.shutdown(Duration::from_secs(5));
    assert_eq!(journal_kinds(jdir.path()), ["session", "intent", "done"]);
}

#[test]
fn changed_items_are_not_deleted() {
    changed_items(PortableBackend::default);
    #[cfg(windows)]
    changed_items(dut_scan::WindowsBackend::default);
}

fn changed_items<B: Backend>(backend: fn() -> B) {
    let tree = fixture();
    let root = tree.path();
    let jdir = tempdir("journal");
    let e = engine(backend(), Some(jdir.path().to_path_buf()), false);
    scan(&e, root);
    let (file, keep, junk) = (node(&e, "file.bin"), node(&e, "keep"), node(&e, "junk"));
    // A file whose size changed, a directory replaced by a new one, a directory swapped for a
    // symlink to another directory.
    write(&root.join("file.bin"), 4_000);
    fs::rename(root.join("keep"), root.join("keep.old")).unwrap();
    fs::create_dir(root.join("keep")).unwrap();
    let elsewhere_dir = tempdir("elsewhere");
    let elsewhere = elsewhere_dir.path();
    write(&elsewhere.join("precious"), 100);
    fs::remove_dir_all(root.join("junk")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(elsewhere, root.join("junk")).unwrap();
    // A junction, which needs no privilege.
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(root.join("junk"))
            .arg(elsewhere)
            .output()
            .unwrap()
            .status
            .success()
    );
    let s = delete(&e, vec![file, keep, junk], Mode::Permanent);
    assert_eq!(s.deleted, 0, "{s:?}");
    assert_eq!(s.stale, 3, "{s:?}");
    assert!(root.join("file.bin").exists() && root.join("keep").exists());
    assert!(
        elsewhere.join("precious").exists(),
        "a symlink target was touched"
    );
    e.shutdown(Duration::from_secs(1));
}

#[test]
fn refusals() {
    refusals_with(PortableBackend::default);
    #[cfg(windows)]
    refusals_with(dut_scan::WindowsBackend::default);
}

fn refusals_with<B: Backend>(backend: fn() -> B) {
    let tree = fixture();
    let root = tree.path();
    // The scan root itself.
    let jdir = tempdir("journal");
    let e = engine(backend(), Some(jdir.path().to_path_buf()), false);
    scan(&e, root);
    let r = e.read(|m| m.tree.node_ref(m.tree.roots().next().unwrap()));
    let s = delete(&e, vec![r], Mode::Permanent);
    assert_eq!((s.deleted, s.refused), (0, 1));
    assert!(root.exists());
    e.shutdown(Duration::from_secs(1));

    // No journal: nothing is deleted.
    let e = engine(backend(), None, false);
    scan(&e, root);
    let s = delete(&e, vec![node(&e, "junk")], Mode::Permanent);
    assert_eq!((s.deleted, s.refused), (0, 1));
    assert!(root.join("junk").exists());
    e.shutdown(Duration::from_secs(1));

    // Read-only: the request is dropped before a plan exists.
    let jdir = tempdir("journal");
    let e = engine(backend(), Some(jdir.path().to_path_buf()), true);
    scan(&e, root);
    e.send(Command::Delete {
        items: vec![node(&e, "junk")],
        mode: Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))));
    assert!(e.read(|m| m.status.last_delete.is_none()));
    assert!(root.join("junk").exists());
    e.shutdown(Duration::from_secs(1));
}
