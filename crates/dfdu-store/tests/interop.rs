//! Interop with ncdu 2: a directory scanned by dfdu and by ncdu gives the same
//! totals, and an export goes through ncdu (`ncdu -e -f FILE -o FILE`) and back unchanged.
//! Skipped when `ncdu` is not installed.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::process::Command as Proc;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_store::ncdu::{self, Limits, Metadata};
use dfdu_tree::Totals;

fn have_ncdu() -> bool {
    Proc::new("ncdu")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn fixture(root: &Path) {
    let w = |p: &str, n: usize| {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![3u8; n]).unwrap();
    };
    w("a/one", 10_000);
    w("a/b/two", 70_000);
    w("a/b/c/three", 1);
    w("empty/.keep", 0);
    w("sp ace/\"quoted\"\\name", 5_000);
    fs::hard_link(root.join("a/one"), root.join("link")).unwrap();
    fs::hard_link(root.join("a/one"), root.join("a/b/link2")).unwrap();
    std::os::unix::fs::symlink("a/one", root.join("sym")).unwrap();
    fs::create_dir(root.join("void")).unwrap();
}

/// The scan's export and root totals.
fn scan(root: &Path) -> (Vec<u8>, Totals) {
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let out = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let mut out = vec![];
        ncdu::export(&m.tree, r, &mut out, &Metadata::default()).unwrap();
        (out, m.tree.total(r))
    });
    e.shutdown(Duration::from_secs(1));
    out
}

fn totals(json: &[u8]) -> Totals {
    let im = ncdu::import(json, &Limits::default()).unwrap();
    im.tree.total(im.root)
}

#[test]
fn ncdu_and_dfdu_agree() {
    if !have_ncdu() {
        eprintln!("skipped: ncdu is not installed");
        return;
    }
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    fixture(&root);
    let (ours, scanned) = scan(&root);
    let dfdu_t = totals(&ours);
    assert_eq!(
        (dfdu_t.apparent, dfdu_t.alloc, dfdu_t.items),
        (scanned.apparent, scanned.alloc, scanned.items)
    );

    // ncdu's own scan of the same directory.
    let theirs = tmp.path().join("ncdu.json");
    let ok = Proc::new("ncdu")
        .args(["-x", "-o"])
        .arg(&theirs)
        .arg(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let ncdu_t = totals(&fs::read(&theirs).unwrap());
    assert_eq!(
        (ncdu_t.apparent, ncdu_t.alloc, ncdu_t.items),
        (scanned.apparent, scanned.alloc, scanned.items),
        "ncdu's scan against dfdu's"
    );

    // Ours through ncdu and back.
    let mine = tmp.path().join("dfdu.json");
    fs::write(&mine, &ours).unwrap();
    let back = tmp.path().join("back.json");
    let ok = Proc::new("ncdu")
        .args(["-e", "-f"])
        .arg(&mine)
        .arg("-o")
        .arg(&back)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let back_t = totals(&fs::read(&back).unwrap());
    assert_eq!(
        (back_t.apparent, back_t.alloc, back_t.items),
        (dfdu_t.apparent, dfdu_t.alloc, dfdu_t.items)
    );
    // Compressed, both ways.
    let zst = tmp.path().join("ncdu.zst");
    let ok = Proc::new("ncdu")
        .args(["-x", "--compress", "-o"])
        .arg(&zst)
        .arg(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(ok.status.success());
    let zst_t = totals(&fs::read(&zst).unwrap());
    assert_eq!(
        (zst_t.apparent, zst_t.alloc, zst_t.items),
        (ncdu_t.apparent, ncdu_t.alloc, ncdu_t.items)
    );
    let mut writer = dfdu_store::ZstdWriter::new(fs::File::create(&mine).unwrap());
    std::io::Write::write_all(&mut writer, &ours).unwrap();
    writer.finish().unwrap();
    let ok = Proc::new("ncdu")
        .arg("-f")
        .arg(&mine)
        .arg("-o")
        .arg(&back)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let back_t = totals(&fs::read(&back).unwrap());
    assert_eq!(
        (back_t.apparent, back_t.alloc, back_t.items),
        (dfdu_t.apparent, dfdu_t.alloc, dfdu_t.items)
    );
}

/// ncdu's binary export (`-O`), with and without the extended fields, reads with the same
/// totals as its JSON export and dfdu's own scan, from a file and from memory. The tree is big
/// enough for several data blocks.
#[test]
fn ncdu_binary_export_reads() {
    if !have_ncdu() {
        eprintln!("skipped: ncdu is not installed");
        return;
    }
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    fixture(&root);
    for d in 0..40 {
        let dir = root.join(format!("many/d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..150 {
            fs::write(
                dir.join(format!("file-with-a-longer-name-{f}")),
                vec![1u8; f],
            )
            .unwrap();
        }
    }
    let (_, scanned) = scan(&root);
    let json = tmp.path().join("ncdu.json");
    let ok = Proc::new("ncdu")
        .args(["-x", "-o"])
        .arg(&json)
        .arg(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(ok.status.success());
    let n = totals(&fs::read(&json).unwrap());
    for extended in [false, true] {
        let bin = tmp.path().join("ncdu.bin");
        let mut cmd = Proc::new("ncdu");
        cmd.arg("-x");
        if extended {
            cmd.arg("-e");
        }
        let ok = cmd
            .arg("-O")
            .arg(&bin)
            .arg(&root)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let from_file =
            dfdu_store::open_file(fs::File::open(&bin).unwrap(), &Limits::default()).unwrap();
        let bytes = fs::read(&bin).unwrap();
        let from_mem = dfdu_store::open(&bytes[..], &Limits::default()).unwrap();
        for o in [from_file, from_mem] {
            let t = o.tree.total(o.root);
            assert_eq!(
                (t.apparent, t.alloc, t.items),
                (n.apparent, n.alloc, n.items),
                "binary (extended {extended}) against JSON"
            );
            assert_eq!(
                (t.apparent, t.alloc, t.items),
                (scanned.apparent, scanned.alloc, scanned.items)
            );
            assert_eq!(o.entries, scanned.items);
        }
    }
}
