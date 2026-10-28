//! Trash mode and its undo on macOS. It uses the real Trash (there is no way to redirect it),
//! so it is ignored by default: `cargo test -p dfdu-engine --test trash_macos -- --ignored`.

#![cfg(target_os = "macos")]

use std::fs;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_scan::{JobOptions, PortableBackend};

#[test]
#[ignore]
fn trash_and_undo() {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    let tmp = tempfile::Builder::new()
        .prefix("dfdu-trash-")
        .tempdir_in(&base)
        .unwrap();
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("junk")).unwrap();
    fs::write(root.join("junk/x.bin"), vec![1u8; 5000]).unwrap();
    let state = tempfile::tempdir().unwrap();
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            journal_dir: Some(state.path().join("ops")),
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
        let c = m.tree.children(r).find(|&c| m.tree.name(c) == b"junk");
        m.tree.node_ref(c.unwrap())
    });
    e.send(Command::Delete {
        items: vec![junk],
        mode: Mode::Trash,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let s = e.read(|m| m.status.last_delete.clone()).unwrap();
    assert_eq!(s.deleted, 1, "{:?}", e.read(|m| m.status.notices.clone()));
    assert!(!root.join("junk").exists());
    e.send(Command::Undo { plan: s.plan });
    while e.read(|m| m.status.undoing.is_some()) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    assert!(
        root.join("junk/x.bin").exists(),
        "{:?}",
        e.read(|m| m.status.notices.clone())
    );
    e.shutdown(Duration::from_secs(1));
}
