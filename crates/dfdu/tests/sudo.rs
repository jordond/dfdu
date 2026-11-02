//! `sudo dfdu` scans as root but leaves its files to the user who ran sudo, and an agent started
//! as the GUI starts one for an administrator's scan sees what `sudo dfdu scan` sees. Runs only
//! as root with `SUDO_UID` set to another user (CI's root btrfs step runs tests through `sudo`);
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

/// The agent of an administrator's scan (`dfdu_session::elevate`, as the GUI starts it, here
/// with `DFDU_ELEVATE_COMMAND` empty since the test already runs as root) reports itself
/// privileged and gives the totals of `sudo dfdu scan`: on the btrfs fixture when
/// `DFDU_BTRFS_DIR` names it (exact sizes, which need root), else on a small tree.
#[test]
fn an_elevated_agent_scans_as_sudo_dfdu_does() {
    use dfdu_engine::{Command, EngineOptions};
    use dfdu_session::elevate;
    use std::time::Duration;

    if dfdu_sys::acting::detect().is_none() {
        return;
    }
    if std::env::var_os(elevate::ELEVATE_VAR).is_none_or(|v| !v.is_empty()) {
        // CI's root step sets it; without it the check would pass there unseen.
        assert!(
            std::env::var_os("DFDU_BTRFS_DIR").is_none(),
            "{} must be set and empty in the root btrfs step",
            elevate::ELEVATE_VAR
        );
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::env::var_os("DFDU_BTRFS_DIR").map_or_else(
        || {
            let d = tmp.path().join("t");
            std::fs::create_dir_all(d.join("a/b")).unwrap();
            std::fs::write(d.join("a/f"), vec![1u8; 10_000]).unwrap();
            std::fs::write(d.join("a/b/g"), vec![2u8; 100_000]).unwrap();
            d
        },
        std::path::PathBuf::from,
    );
    let state = tmp.path().join("state");

    let out = common::bin_in(&state)
        .args(["scan", "--format", "json"])
        .arg(&root)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&common::ok(out)).unwrap();
    let totals = &json["totals"];
    let want = ["apparent", "disk", "du", "entries"].map(|k| totals[k].as_u64().unwrap());

    let agent = [env!("CARGO_BIN_EXE_dfdu").into(), "agent".into()];
    let io = elevate::spawn(&agent).unwrap();
    let el = elevate::connect(
        io,
        &root,
        None,
        EngineOptions::default(),
        dfdu_scan::JobOptions::default(),
    )
    .unwrap();
    assert!(el.privileged, "the agent runs as root");
    assert_eq!(el.errors, Vec::<String>::new());
    let e = el.engine;
    e.send(Command::Scan {
        path: el.path,
        opts: el.job,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(60))).is_idle());
    let got = e.read(|m| {
        assert_eq!(m.status.fatal, None);
        let top = m.tree.roots().next().unwrap();
        let t = m.tree.total(top);
        [t.apparent, t.alloc, t.referenced, t.items]
    });
    e.shutdown(Duration::from_secs(5));
    assert_eq!(got, want, "apparent, disk, du, entries");
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
