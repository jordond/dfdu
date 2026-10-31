//! Staging across sessions: a second instance started while the first has items staged, and a
//! session killed part-way through a staged delete, then recovery. The second re-runs this
//! binary as the session to kill, with the `fault-injection` hook that stops a staged delete after its first entry.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use dfdu_clean::recover::{self, Pending};
use dfdu_clean::{DeleterMsg, DeleterReq, Mode, Plan, spawn_deleter};
use dfdu_tree::{Kind, Tree};

use common::canonical_tempdir;

const CHILD: &str = "DFDU_RECOVER_CHILD";

/// A tree of `n` files at `root/junk`, and a plan to stage it.
fn plan(root: &Path, n: usize) -> Plan {
    let junk = root.join("junk");
    fs::create_dir_all(&junk).unwrap();
    for i in 0..n {
        fs::write(junk.join(format!("f{i}")), b"x").unwrap();
    }
    let identity = common::identity(&junk).unwrap();
    assert_eq!(identity.kind, Kind::Dir);
    Plan {
        id: 1,
        mode: Mode::Stage,
        scan_root: root.into(),
        items: vec![common::item(&mut Tree::new(), junk, identity)],
        protected: None,
    }
}

fn until_done(rx: &Receiver<DeleterMsg>) {
    for m in rx {
        if let DeleterMsg::Item { result, .. } = &m {
            assert_eq!(*result, dfdu_clean::DeleteResult::Deleted);
        }
        if matches!(m, DeleterMsg::Done { .. }) {
            return;
        }
    }
    panic!("deleter stopped");
}

fn staged_dir(state: &Path) -> Vec<PathBuf> {
    fs::read_dir(state.join("stage"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|s| fs::read_dir(s.path()).unwrap().flatten().map(|e| e.path()))
        .collect()
}

/// While the first session holds its journal, recovery in a second one sees
/// nothing of it and leaves its staged item alone; once it has exited, the item is reported as
/// staged and can be put back.
#[test]
fn second_instance_leaves_a_live_session_alone() {
    let tmp = canonical_tempdir();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    let (tx, rx) = unbounded();
    let d = spawn_deleter(Some(state.join("ops")), Duration::from_secs(3600), tx);
    d.reqs.send(DeleterReq::Plan(plan(&root, 3))).unwrap();
    until_done(&rx);
    let staged = staged_dir(&state);
    assert_eq!(staged.len(), 1);

    let r = recover::scan(&state.join("ops")).unwrap();
    assert!(r.items.is_empty(), "a live session's items: {:?}", r.items);
    drop(r);
    assert!(
        staged[0].join("f2").exists(),
        "the live session's staged item was touched"
    );

    drop(d.reqs);
    d.thread.join().unwrap();
    let mut r = recover::scan(&state.join("ops")).unwrap();
    assert_eq!(r.items.len(), 1);
    assert_eq!(
        r.items[0].state,
        Pending::Staged {
            stage: staged[0].clone()
        }
    );
    assert_eq!(r.restore(0).unwrap(), root.join("junk"));
    assert!(root.join("junk/f2").exists());
    drop(r);
    assert!(recover::scan(&state.join("ops")).unwrap().items.is_empty());
}

/// Finishing a staged item deletes only that item: something else put at its stage path since
/// the session ended is left alone, and the item can be finished once it is back.
#[test]
fn finish_leaves_a_swapped_stage_path_alone() {
    let tmp = canonical_tempdir();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    let (tx, rx) = unbounded();
    let d = spawn_deleter(Some(state.join("ops")), Duration::from_secs(3600), tx);
    d.reqs.send(DeleterReq::Plan(plan(&root, 3))).unwrap();
    until_done(&rx);
    drop(d.reqs);
    d.thread.join().unwrap();
    let staged = staged_dir(&state);
    assert_eq!(staged.len(), 1);
    let aside = tmp.path().join("aside");
    fs::rename(&staged[0], &aside).unwrap();
    fs::create_dir(&staged[0]).unwrap();
    fs::write(staged[0].join("precious"), b"p").unwrap();

    let mut r = recover::scan(&state.join("ops")).unwrap();
    assert_eq!(r.items.len(), 1);
    assert!(r.finish(0).is_err());
    assert!(staged[0].join("precious").exists());
    assert_eq!(r.items.len(), 1);
    drop(r);

    fs::remove_dir_all(&staged[0]).unwrap();
    fs::rename(&aside, &staged[0]).unwrap();
    let mut r = recover::scan(&state.join("ops")).unwrap();
    r.finish(0).unwrap();
    assert!(!staged[0].exists());
    drop(r);
    assert!(recover::scan(&state.join("ops")).unwrap().items.is_empty());
}

/// Killed during a staged delete, the item is reported as partly deleted; it can
/// be finished, never restored as if whole.
#[test]
fn killed_during_a_staged_delete() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let tmp = canonical_tempdir();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    let mut child = Proc::new(std::env::current_exe().unwrap())
        .args(["child_session", "--exact", "--nocapture"])
        .env(CHILD, tmp.path())
        .env("DFDU_FAULT_COMMIT_HANG", "1")
        .spawn()
        .unwrap();
    let ops = state.join("ops");
    let deadline = Instant::now() + Duration::from_secs(20);
    // The hook deletes one entry and then waits. The child's journal is not read here: it is
    // locked, and on Windows that keeps other handles from reading it. Its commit intent is
    // written before the first entry goes, and recovery's verdict below shows it was.
    let staged = loop {
        let s = staged_dir(&state);
        if s.len() == 1 && fs::read_dir(&s[0]).unwrap().count() == 4 {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "the child never started deleting its staged item"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    child.kill().unwrap();
    child.wait().unwrap();

    let mut r = recover::scan(&ops).unwrap();
    assert_eq!(r.items.len(), 1, "{:?}", r.items);
    assert_eq!(
        r.items[0].state,
        Pending::PartlyDeleted {
            stage: staged[0].clone()
        }
    );
    assert!(r.items[0].describe().contains("partly deleted"));
    let e = r.restore(0).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(e.to_string().contains("only be finished"), "{e}");
    r.finish(0).unwrap();
    assert!(!staged[0].exists());
    assert!(!root.join("junk").exists());
    drop(r);
    assert!(recover::scan(&ops).unwrap().items.is_empty());
}

/// The session `killed_during_a_staged_delete` kills: stages `junk`, then deletes it with the
/// hook that stops part-way.
#[test]
fn child_session() {
    let Some(base) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    let (tx, rx) = unbounded();
    let d = spawn_deleter(Some(base.join("state/ops")), Duration::from_secs(3600), tx);
    d.reqs
        .send(DeleterReq::Plan(plan(&base.join("tree"), 5)))
        .unwrap();
    until_done(&rx);
    d.reqs.send(DeleterReq::Commit).unwrap();
    // Killed while the commit waits.
    std::thread::sleep(Duration::from_secs(60));
    panic!("not killed");
}
