//! The reclaimable estimate against the free space a delete actually gives back (ROADMAP M6:
//! within 1% on fixtures without clones). It needs a filesystem nothing else writes to: on
//! macOS the test makes an HFS+ disk image (no clones) and mounts it; elsewhere it runs in
//! `DFDU_QUIET_FS` if that is set (a tmpfs, say) and is skipped otherwise.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_scan::{JobOptions, PortableBackend};

fn available(p: &Path) -> u64 {
    let s = rustix::fs::statvfs(p).unwrap();
    s.f_bavail * s.f_frsize
}

/// A mounted disk image, detached on drop.
#[cfg(target_os = "macos")]
struct Image {
    mount: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl Image {
    fn new() -> Option<Image> {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("quiet.dmg");
        let mount = dir.path().join("mnt");
        let run = |args: &[&std::ffi::OsStr]| {
            std::process::Command::new("hdiutil")
                .args(args)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let ok = run(&[
            "create".as_ref(),
            "-size".as_ref(),
            "64m".as_ref(),
            "-fs".as_ref(),
            "HFS+".as_ref(),
            "-volname".as_ref(),
            "dfduquiet".as_ref(),
            img.as_os_str(),
        ]) && run(&[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            img.as_os_str(),
        ]);
        ok.then(|| Image { mount, _dir: dir })
    }
}

#[cfg(target_os = "macos")]
impl Drop for Image {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .arg("detach")
            .arg("-force")
            .arg(&self.mount)
            .output();
    }
}

fn write(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; bytes]).unwrap();
}

#[test]
fn estimate_matches_freed_space() {
    #[cfg(target_os = "macos")]
    let image = Image::new();
    #[cfg(target_os = "macos")]
    let base: Option<PathBuf> = image.as_ref().map(|i| i.mount.clone());
    #[cfg(not(target_os = "macos"))]
    let base: Option<PathBuf> = std::env::var_os("DFDU_QUIET_FS").map(PathBuf::from);
    let Some(base) = base else {
        eprintln!("skipped: no quiet filesystem (set DFDU_QUIET_FS)");
        return;
    };
    let root = dunce::canonicalize(&base).unwrap().join("tree");
    // Sizes off block boundaries, nested directories, and hard links: one inode with both
    // links inside the targets (freed), one with a link outside them (not freed), one with
    // links in two different targets (freed).
    for i in 0..40 {
        write(&root.join(format!("a/d{}/f{i}", i % 7)), 50_000 + i * 3_001);
        write(&root.join(format!("b/e{}/g{i}", i % 5)), 70_000 + i * 1_777);
    }
    write(&root.join("keep/k.bin"), 400_000);
    write(&root.join("a/both.bin"), 900_000);
    fs::hard_link(root.join("a/both.bin"), root.join("a/d1/both.link")).unwrap();
    write(&root.join("a/out.bin"), 800_000);
    fs::hard_link(root.join("a/out.bin"), root.join("keep/out.link")).unwrap();
    write(&root.join("a/across.bin"), 700_000);
    fs::hard_link(root.join("a/across.bin"), root.join("b/across.link")).unwrap();

    let state = tempfile::tempdir().unwrap();
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            journal_dir: Some(state.path().join("ops")),
            check_invariants: true,
            workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let items = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        m.tree
            .children(r)
            .filter(|&c| m.tree.name(c) != b"keep")
            .map(|c| m.tree.node_ref(c))
            .collect::<Vec<_>>()
    });
    e.send(Command::Estimate {
        id: 7,
        items: items.clone(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))));
    let est = e.read(|m| m.status.estimate.clone()).unwrap();
    assert_eq!((est.id, est.items, est.scanning), (7, 2, false));
    assert!(est.linked_out >= 800_000, "{est:?}");

    let before = available(&base);
    e.send(Command::Delete {
        items,
        mode: Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    assert_eq!(e.read(|m| m.status.last_delete.clone()).unwrap().deleted, 2);
    rustix::fs::sync();
    // HFS+ returns blocks when its journal is flushed; give it a moment.
    let mut freed = 0;
    for _ in 0..50 {
        freed = available(&base).saturating_sub(before);
        if freed.abs_diff(est.bytes) <= est.bytes / 100 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        freed.abs_diff(est.bytes) <= est.bytes / 100,
        "estimated {} freed {freed} ({est:?})",
        est.bytes
    );
    eprintln!("estimated {} freed {freed}", est.bytes);
    assert!(root.join("keep/out.link").exists());
    e.shutdown(Duration::from_secs(1));
}
