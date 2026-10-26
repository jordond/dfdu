//! Trash mode on Linux, with the freedesktop trash redirected to a temporary directory so the
//! test never touches the user's real trash. The outer test re-runs this binary with
//! `XDG_DATA_HOME` set, because changing the environment of a running process needs `unsafe`.

#![cfg(all(unix, not(target_os = "macos")))]

use std::fs;
use std::path::PathBuf;
use std::process::Command as Proc;
use std::time::Duration;

use dut_engine::{Command, Engine, EngineOptions, Mode};
use dut_scan::{JobOptions, PortableBackend};

const INNER: &str = "DUT_TRASH_TEST_BASE";

#[test]
fn trash_moves_item_into_trash() {
    if std::env::var_os(INNER).is_some() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    fs::create_dir_all(base.join("data")).unwrap();
    let out = Proc::new(std::env::current_exe().unwrap())
        .args(["inner_trash", "--exact", "--nocapture"])
        .env(INNER, base)
        .env("XDG_DATA_HOME", base.join("data"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn inner_trash() {
    let Some(base) = std::env::var_os(INNER).map(PathBuf::from) else {
        return;
    };
    let root = base.join("tree");
    fs::create_dir_all(root.join("junk")).unwrap();
    fs::write(root.join("junk/x.bin"), vec![1u8; 5000]).unwrap();
    let root = root.canonicalize().unwrap();
    let e = Engine::start(
        PortableBackend,
        EngineOptions {
            journal_dir: Some(base.join("state")),
            workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let junk = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        m.tree.node_ref(
            m.tree
                .children(r)
                .find(|&c| m.tree.name(c) == b"junk")
                .unwrap(),
        )
    });
    e.send(Command::Delete {
        items: vec![junk],
        mode: Mode::Trash,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let s = e.read(|m| m.status.last_delete.clone()).unwrap();
    assert_eq!(
        s.deleted,
        1,
        "{s:?} {:?}",
        e.read(|m| m.status.notices.clone())
    );
    assert!(!root.join("junk").exists());
    assert!(
        base.join("data/Trash/files/junk/x.bin").exists(),
        "item is in the redirected trash"
    );
    e.shutdown(Duration::from_secs(1));
}
