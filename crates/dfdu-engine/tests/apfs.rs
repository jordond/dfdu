//! APFS clones (`cp -c`) share their data: a group of clones counts it once, at the lowest
//! directory holding them all, as reflinks do on btrfs. Skipped where the temporary directory
//! is not on APFS.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::process::Command as Proc;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, NativeBackend};

mod common;
use common::child;

fn clone(from: &Path, to: &Path) {
    let st = Proc::new("cp")
        .arg("-c")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn clones_count_once() {
    let tmp = common::tempdir();
    let root = tmp.path().join("t");
    if !dfdu_sys::shares_extents(tmp.path()).unwrap() {
        eprintln!("skipped: not APFS");
        return;
    }
    let big: Vec<u8> = (0..4_000_000u32)
        .map(|i| (i * 7 + i / 991).to_le_bytes()[0])
        .collect();
    fs::create_dir_all(root.join("x")).unwrap();
    fs::create_dir_all(root.join("y")).unwrap();
    fs::write(root.join("x/a"), &big).unwrap();
    clone(&root.join("x/a"), &root.join("x/b"));
    clone(&root.join("x/a"), &root.join("y/c"));
    fs::write(root.join("plain"), &big).unwrap();
    // Below the size measured: counted per file, as du does.
    fs::write(root.join("small"), &big[..100_000]).unwrap();
    clone(&root.join("small"), &root.join("small2"));
    let alloc = |p: &str| {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(root.join(p)).unwrap().blocks() * 512
    };
    let (big_alloc, small_alloc) = (alloc("x/a"), alloc("small"));

    let e = Engine::start(
        NativeBackend::default(),
        EngineOptions {
            journal_dir: None,
            check_invariants: true,
            workers: 2,
            measure_workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    e.read(|m| {
        let t = &m.tree;
        let r = t.roots().next().unwrap();
        let files = 2 * big_alloc + 2 * small_alloc;
        let dirs: u64 = [r, child(t, r, "x"), child(t, r, "y")]
            .iter()
            .map(|&d| t.raw_size(d).1)
            .sum();
        assert_eq!(t.total(r).alloc, files + dirs);
        // Shared by x/a and x/b alone would be homed at x; y/c moves it to the root.
        assert!(t.total(child(t, r, "x")).alloc < big_alloc);
        assert_eq!(m.status.unmeasured, 0);
    });
    e.shutdown(Duration::from_secs(1));
}
