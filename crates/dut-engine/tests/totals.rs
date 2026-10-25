//! Totals against an independent reference walker (and `du` where available) on a fixture
//! with hard links, symlinks and a loop, a sparse file, a non-UTF-8 name, a path longer than
//! 4096 bytes and an unreadable directory (ROADMAP M1 acceptance).

#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{JobOptions, PortableBackend};
use dut_tree::{Ix, Totals, Tree};

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("dut-totals-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    for (p, n) in [
        ("a/f1", 5000),
        ("a/b/f2", 70_000),
        ("a/b/c/f3", 1),
        ("top", 123),
    ] {
        fs::write(root.join(p), vec![3u8; n]).unwrap();
    }
    // Hard links: three inside the tree, one inode also linked from outside it.
    fs::write(root.join("a/h1"), vec![9u8; 40_000]).unwrap();
    fs::hard_link(root.join("a/h1"), root.join("a/b/h2")).unwrap();
    fs::hard_link(root.join("a/h1"), root.join("empty/../h3")).unwrap();
    let outside = std::env::temp_dir().join(format!("dut-totals-out-{}", std::process::id()));
    fs::write(root.join("x1"), vec![1u8; 12_000]).unwrap();
    let _ = fs::remove_file(&outside);
    fs::hard_link(root.join("x1"), &outside).unwrap();
    // A sparse file: large apparent size, little allocated.
    let f = fs::File::create(root.join("sparse")).unwrap();
    f.set_len(64 * 1024 * 1024).unwrap();
    // Symlinks, including a loop; they are counted as links, never followed.
    std::os::unix::fs::symlink("a", root.join("link-to-a")).unwrap();
    std::os::unix::fs::symlink(".", root.join("a/loop")).unwrap();
    // A name that is not UTF-8.
    use std::os::unix::ffi::OsStrExt;
    // (APFS refuses such names with EILSEQ; the case is then skipped there.)
    match fs::write(
        root.join(std::ffi::OsStr::from_bytes(b"bad-\xff\xfe-name")),
        b"x",
    ) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(92) => {}
        Err(e) => panic!("{e}"),
    }
    // A path longer than 4096 bytes, built one level at a time relative to the current
    // directory (this test binary has a single test, so changing it is safe). The portable
    // backend opens directories by full path and reports the levels past PATH_MAX as errors;
    // the openat-based Linux backend (M2) will read them.
    fs::create_dir(root.join("deep")).unwrap();
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(root.join("deep")).unwrap();
    for i in 0..25 {
        let name = format!("{i:03}-{}", "x".repeat(200));
        fs::create_dir(&name).unwrap();
        std::env::set_current_dir(&name).unwrap();
    }
    fs::write("leaf", vec![5u8; 777]).unwrap();
    std::env::set_current_dir(cwd).unwrap();
    // An unreadable directory (readable anyway when the tests run as root).
    fs::create_dir_all(root.join("locked/inner")).unwrap();
    fs::write(root.join("locked/inner/secret"), b"s").unwrap();
    let mut perm = fs::metadata(root.join("locked")).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o000);
    fs::set_permissions(root.join("locked"), perm).unwrap();
    root.canonicalize().unwrap()
}

/// Reference: a plain recursive walk with the same metadata source as the scanner (the
/// parent's listing), hard links charged to their lowest path (byte order).
fn reference(root: &Path) -> HashMap<Vec<u8>, Totals> {
    use std::os::unix::ffi::OsStrExt;
    let root_md = fs::metadata(root).unwrap();
    let dev = root_md.dev();
    let mut entries: Vec<(Vec<u8>, fs::Metadata)> = vec![];
    let mut errors: HashMap<Vec<u8>, u64> = HashMap::new();
    let mut stack = vec![(root.to_path_buf(), root_md)];
    while let Some((d, md)) = stack.pop() {
        entries.push((d.as_os_str().as_bytes().to_vec(), md));
        match fs::read_dir(&d) {
            Err(_) => {
                errors.insert(d.as_os_str().as_bytes().to_vec(), 1);
            }
            Ok(rd) => {
                for e in rd {
                    let e = e.unwrap();
                    let m = e.metadata().unwrap();
                    if m.is_dir() && m.dev() == dev {
                        stack.push((e.path(), m));
                    } else {
                        entries.push((e.path().as_os_str().as_bytes().to_vec(), m));
                    }
                }
            }
        }
    }
    let mut lowest: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
    for (p, m) in &entries {
        if !m.is_dir() && m.nlink() > 1 {
            let e = lowest
                .entry((m.dev(), m.ino()))
                .or_insert_with(|| p.clone());
            if p < e {
                *e = p.clone();
            }
        }
    }
    let mut totals: HashMap<Vec<u8>, Totals> = HashMap::new();
    for (p, m) in &entries {
        let counted = m.is_dir() || m.nlink() <= 1 || lowest.get(&(m.dev(), m.ino())) == Some(p);
        let own = Totals {
            apparent: if counted { m.len() } else { 0 },
            alloc: if counted { m.blocks() * 512 } else { 0 },
            items: 1,
            errors: *errors.get(p).unwrap_or(&0),
        };
        // Add to the entry itself and every ancestor inside the root.
        let mut cur = Some(PathBuf::from(std::ffi::OsStr::from_bytes(p)));
        while let Some(c) = cur {
            let key = c.as_os_str().as_bytes().to_vec();
            totals.entry(key).or_default().add(&own);
            if c == root {
                break;
            }
            cur = c.parent().map(Path::to_path_buf);
        }
    }
    totals
}

fn walk(tree: &Tree, ix: Ix, out: &mut HashMap<Vec<u8>, Totals>) {
    out.insert(tree.path(ix), tree.total(ix));
    for c in tree.children(ix) {
        walk(tree, c, out);
    }
}

#[test]
fn totals_match_reference_and_du() {
    let root = fixture();
    let e = Engine::start(
        PortableBackend,
        EngineOptions {
            check_invariants: true,
            workers: 4,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))));
    let ours = e.read(|m| {
        let mut out = HashMap::new();
        walk(&m.tree, m.tree.roots().next().unwrap(), &mut out);
        out
    });
    let want = reference(&root);
    let mut diffs = vec![];
    for (p, t) in &want {
        match ours.get(p) {
            Some(o) if o == t => {}
            other => diffs.push(format!(
                "{}: ours {other:?} want {t:?}",
                String::from_utf8_lossy(p)
            )),
        }
    }
    assert!(
        ours.len() == want.len() && diffs.is_empty(),
        "{} entries vs {}; {diffs:#?}",
        ours.len(),
        want.len()
    );

    // GNU du, without the subtree past PATH_MAX (du reads it with openat; the portable backend
    // cannot yet, see the fixture).
    if let Ok(out) = std::process::Command::new("du")
        .args(["-s", "-B1", "-x", "--exclude=deep"])
        .arg(&root)
        .output()
        && !out.stdout.is_empty()
    {
        let du: u64 = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        use std::os::unix::ffi::OsStrExt;
        let ours_root = ours[root.as_os_str().as_bytes()].alloc
            - ours[root.join("deep").as_os_str().as_bytes()].alloc;
        assert_eq!(ours_root, du, "disk usage outside deep/ differs from du");
    }
    e.shutdown(Duration::from_secs(1));
    let mut perm = fs::metadata(root.join("locked")).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    let _ = fs::set_permissions(root.join("locked"), perm);
    let _ = fs::remove_dir_all(&root);
}
