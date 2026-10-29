//! A directory above a planned item replaced by a link after the scan (a symlink on Unix, a
//! junction on Windows, where canonicalisation resolves links differently): the item is
//! reported stale and nothing is deleted, neither at its path nor where the link leads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use crossbeam_channel::unbounded;
use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Mode, Plan, spawn_deleter};
use dfdu_tree::Tree;

use common::{canonical_tempdir, identity, item};

#[cfg(unix)]
fn link_dir(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn link_dir(link: &Path, target: &Path) {
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "mklink /J {}", link.display());
}

#[test]
fn an_ancestor_replaced_by_a_link_leaves_the_item_stale() {
    let tmp = canonical_tempdir();
    let root = tmp.path().join("root");
    let victim = root.join("a/b/item");
    fs::create_dir_all(victim.parent().unwrap()).unwrap();
    fs::write(&victim, b"scanned").unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join("b")).unwrap();
    fs::write(elsewhere.join("b/item"), b"keep").unwrap();

    let mut tree = Tree::new();
    let it = item(&mut tree, victim.clone(), identity(&victim).unwrap());
    // After the scan, `a` is swapped for a link to a directory with the same layout.
    fs::remove_dir_all(root.join("a")).unwrap();
    link_dir(&root.join("a"), &elsewhere);

    let (tx, rx) = unbounded();
    let d = spawn_deleter(Some(tmp.path().join("ops")), Duration::from_secs(60), tx);
    d.reqs
        .send(DeleterReq::Plan(Plan {
            id: 1,
            mode: Mode::Permanent,
            scan_root: root.as_path().into(),
            items: vec![it],
            protected: None,
        }))
        .unwrap();
    let result = loop {
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DeleterMsg::Item { result, .. } => break result,
            DeleterMsg::Done { .. } => panic!("no result for the item"),
            _ => {}
        }
    };
    assert_eq!(
        result,
        DeleteResult::Stale("a directory above it is now a symlink".into())
    );
    assert_eq!(fs::read(elsewhere.join("b/item")).unwrap(), b"keep");
}
