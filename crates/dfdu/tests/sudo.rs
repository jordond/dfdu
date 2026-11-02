//! `sudo dfdu` scans as root but leaves its files to the user who ran sudo. Runs only as root
//! with `SUDO_UID` set to another user (CI's root btrfs step runs tests through `sudo`);
//! skipped (passing) otherwise.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::os::unix::fs::MetadataExt;

#[test]
fn files_made_under_sudo_belong_to_the_user() {
    let Some(user) = dfdu_sys::acting::detect() else {
        return;
    };
    let top = user
        .home
        .join(format!(".dfdu-sudo-test-{}", std::process::id()));
    let state = top.join("state");
    let tree = tempfile::tempdir().unwrap();
    std::fs::write(tree.path().join("f"), vec![0u8; 4096]).unwrap();
    // Root's HOME, as Linux sudo sets it: dfdu executes itself again with the user's.
    let out = common::bin_in(&state)
        .env("HOME", "/root")
        .args(["scan", "--format", "json"])
        .arg(tree.path())
        .output()
        .unwrap();
    common::ok(out);
    let history = state.join("history.sqlite3");
    assert!(history.exists(), "the scan was recorded");
    for p in [top.clone(), state.clone(), history] {
        assert_eq!(
            std::fs::symlink_metadata(&p).unwrap().uid(),
            user.uid,
            "{}",
            p.display()
        );
    }
    for e in walk(&top) {
        assert_eq!(e.1, user.uid, "{}", e.0.display());
    }
    std::fs::remove_dir_all(&top).unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<(std::path::PathBuf, u32)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let m = std::fs::symlink_metadata(e.path()).unwrap();
        v.push((e.path(), m.uid()));
        if m.is_dir() {
            v.extend(walk(&e.path()));
        }
    }
    v
}
