//! Totals against an independent reference walker (and `du` where available) on a fixture
//! with hard links, symlinks and a loop, a sparse file, a name that is not valid UTF-8 (UTF-16
//! on Windows), a path longer than 4096 bytes and an unreadable directory (ROADMAP M1
//! acceptance), with the portable backend and the native one (Linux, macOS, Windows).

#![cfg(any(unix, windows))]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{Backend, JobOptions, PortableBackend};
use dfdu_tree::{Ix, Totals, Tree};
use tempfile::TempDir;

/// A temporary directory under the canonical temp dir, so its path matches the scan root the
/// engine reports (on macOS the temp dir is behind the /var symlink).
fn tempdir() -> TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

/// A path as the tree stores it.
fn key(p: &Path) -> Vec<u8> {
    dfdu_tree::name_bytes(p.as_os_str()).into_owned()
}

/// Returns the tree and a directory outside it that holds a hard link into it.
fn fixture() -> (TempDir, TempDir) {
    let tmp = tempdir();
    let root = tmp.path();
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
    let outside = tempdir();
    fs::write(root.join("x1"), vec![1u8; 12_000]).unwrap();
    fs::hard_link(root.join("x1"), outside.path().join("x1")).unwrap();
    // A sparse file: large apparent size, little allocated (on NTFS, which allocates the
    // extension unless the file is marked sparse, all of it).
    let f = fs::File::create(root.join("sparse")).unwrap();
    f.set_len(64 * 1024 * 1024).unwrap();
    links(root);
    // A name that is not UTF-8 (APFS refuses such names with EILSEQ; the case is then skipped
    // there), on Windows one with an unpaired surrogate.
    #[cfg(unix)]
    let bad = {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b"bad-\xff\xfe-name").to_os_string()
    };
    #[cfg(windows)]
    let bad = {
        use std::os::windows::ffi::OsStringExt;
        let w: Vec<u16> = "bad-".encode_utf16().chain([0xDC00, 0x2D]).collect();
        std::ffi::OsString::from_wide(&w)
    };
    match fs::write(root.join(bad), b"x") {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(92) => {}
        Err(e) => panic!("{e}"),
    }
    deep(root);
    // An unreadable directory (readable anyway when the tests run as root).
    fs::create_dir_all(root.join("locked/inner")).unwrap();
    fs::write(root.join("locked/inner/secret"), b"s").unwrap();
    lock(&root.join("locked"), true);
    (tmp, outside)
}

/// Symlinks, including a loop; they are counted as links, never followed.
#[cfg(unix)]
fn links(root: &Path) {
    std::os::unix::fs::symlink("a", root.join("link-to-a")).unwrap();
    std::os::unix::fs::symlink(".", root.join("a/loop")).unwrap();
}

/// Symlinks (when the process may create them: an elevated one or Developer Mode), and a
/// junction, which needs no privilege. None is followed.
#[cfg(windows)]
fn links(root: &Path) {
    let _ = std::os::windows::fs::symlink_dir("a", root.join("link-to-a"));
    let _ = std::os::windows::fs::symlink_dir(".", root.join("a/loop"));
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(root.join("junction-to-a"))
        .arg(root.join("a"))
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(made, "mklink /J failed");
}

/// A path longer than 4096 bytes, built one level at a time relative to the current
/// directory (this test binary has a single test, so changing it is safe). The portable
/// backend opens directories by full path and reports the levels past PATH_MAX as errors;
/// the Linux and macOS backends open long paths in pieces and read them.
#[cfg(unix)]
fn deep(root: &Path) {
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
}

/// The same path past `MAX_PATH` on Windows, by full path: std and the Windows backend both
/// open long paths with the `\\?\` prefix, so both read it.
#[cfg(windows)]
fn deep(root: &Path) {
    let mut p = root.join("deep");
    fs::create_dir(&p).unwrap();
    for i in 0..25 {
        p.push(format!("{i:03}-{}", "x".repeat(200)));
        fs::create_dir(&p).unwrap();
    }
    fs::write(p.join("leaf"), vec![5u8; 777]).unwrap();
}

#[cfg(unix)]
fn lock(dir: &Path, locked: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(dir).unwrap().permissions();
    perm.set_mode(if locked { 0o000 } else { 0o755 });
    let _ = fs::set_permissions(dir, perm);
}

/// Denies listing the directory to everyone (`icacls`); the reference notices if it did not
/// take effect.
#[cfg(windows)]
fn lock(dir: &Path, locked: bool) {
    let args: &[&str] = if locked {
        &["/deny", "*S-1-1-0:(RD)"]
    } else {
        &["/remove:d", "*S-1-1-0"]
    };
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args(args)
        .output();
}

/// What the reference uses of an entry.
struct Ent {
    dir: bool,
    len: u64,
    alloc: u64,
    id: (u64, u64),
    nlink: u64,
}

#[cfg(unix)]
fn ent(_path: &Path, m: &fs::Metadata) -> Ent {
    use std::os::unix::fs::MetadataExt;
    Ent {
        dir: m.is_dir(),
        len: m.len(),
        alloc: m.blocks() * 512,
        id: (m.dev(), m.ino()),
        nlink: m.nlink(),
    }
}

/// std has no allocation size, file id or link count on Windows; they come from the entry
/// opened for its attributes (`dfdu_sys::stat_path`). A file's size is std's from the file
/// itself (`symlink_metadata`), which a directory entry's copy may lag for hard links.
#[cfg(windows)]
fn ent(path: &Path, m: &fs::Metadata) -> Ent {
    let w = dfdu_sys::stat_path(path, false).unwrap();
    let id = dfdu_tree::Meta::from_windows(&w);
    let file = m.is_file();
    Ent {
        dir: m.is_dir(),
        len: if file {
            fs::symlink_metadata(path).unwrap().len()
        } else {
            m.len()
        },
        alloc: w.alloc,
        id: (id.dev, id.ino),
        nlink: if file { w.nlink as u64 } else { 1 },
    }
}

/// Reference: a plain recursive walk with the same metadata source as the scanner (the
/// parent's listing), hard links charged to their lowest path (byte order).
fn reference(root: &Path) -> HashMap<Vec<u8>, Totals> {
    let root_md = ent(root, &fs::metadata(root).unwrap());
    let dev = root_md.id.0;
    let mut entries: Vec<(Vec<u8>, Ent)> = vec![];
    let mut errors: HashMap<Vec<u8>, u64> = HashMap::new();
    let mut stack = vec![(root.to_path_buf(), root_md)];
    while let Some((d, md)) = stack.pop() {
        entries.push((key(&d), md));
        match fs::read_dir(&d) {
            Err(_) => {
                errors.insert(key(&d), 1);
            }
            Ok(rd) => {
                for e in rd {
                    let e = e.unwrap();
                    let m = ent(&e.path(), &e.metadata().unwrap());
                    if m.dir && m.id.0 == dev {
                        stack.push((e.path(), m));
                    } else {
                        entries.push((key(&e.path()), m));
                    }
                }
            }
        }
    }
    let mut lowest: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
    for (p, m) in &entries {
        if !m.dir && m.nlink > 1 {
            let e = lowest.entry(m.id).or_insert_with(|| p.clone());
            if p < e {
                *e = p.clone();
            }
        }
    }
    let mut totals: HashMap<Vec<u8>, Totals> = HashMap::new();
    for (p, m) in &entries {
        let counted = m.dir || m.nlink <= 1 || lowest.get(&m.id) == Some(p);
        let own = Totals {
            apparent: if counted { m.len } else { 0 },
            alloc: if counted { m.alloc } else { 0 },
            referenced: if counted { m.alloc } else { 0 },
            items: 1,
            errors: *errors.get(p).unwrap_or(&0),
            ..Totals::default()
        };
        // Add to the entry itself and every ancestor inside the root.
        let mut cur = Some(PathBuf::from(&*dfdu_tree::name_os(p)));
        while let Some(c) = cur {
            totals.entry(key(&c)).or_default().add(&own);
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

fn scan<B: Backend>(backend: B, root: &Path) -> HashMap<Vec<u8>, Totals> {
    let e = Engine::start(
        backend,
        EngineOptions {
            check_invariants: true,
            workers: 4,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))));
    let ours = e.read(|m| {
        let mut out = HashMap::new();
        walk(&m.tree, m.tree.roots().next().unwrap(), &mut out);
        out
    });
    e.shutdown(Duration::from_secs(1));
    ours
}

/// `du -s -B1 -x` of `root`, with extra arguments; `None` without GNU du.
#[cfg(unix)]
fn du(root: &Path, extra: &[&str]) -> Option<u64> {
    let out = std::process::Command::new("du")
        .args(["-s", "-B1", "-x"])
        .args(extra)
        .arg(root)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Entries of `ours` and `want` that differ, leaving out `skip` and everything below it.
fn diffs(
    ours: &HashMap<Vec<u8>, Totals>,
    want: &HashMap<Vec<u8>, Totals>,
    skip: &[u8],
) -> Vec<String> {
    let below = |p: &[u8]| {
        !skip.is_empty()
            && p.starts_with(skip)
            && (p.len() == skip.len() || p[skip.len()] == dfdu_tree::SEP)
    };
    let mut out = vec![];
    for (p, t) in want.iter().filter(|(p, _)| !below(p)) {
        match ours.get(p) {
            Some(o) if o == t => {}
            other => out.push(format!(
                "{}: ours {other:?} want {t:?}",
                String::from_utf8_lossy(p)
            )),
        }
    }
    let extra = ours.keys().filter(|p| !below(p) && !want.contains_key(*p));
    out.extend(extra.map(|p| format!("{}: not in the reference", String::from_utf8_lossy(p))));
    out
}

#[test]
fn totals_match_reference_and_du() {
    let (tmp, _outside) = fixture();
    let root = tmp.path().to_path_buf();
    let want = reference(&root);
    let deep = key(&root.join("deep"));

    let ours = scan(PortableBackend::default(), &root);
    let d = diffs(&ours, &want, b"");
    assert!(d.is_empty(), "portable backend: {d:#?}");
    assert!(ours.contains_key(&deep));
    // GNU du, without the subtree past PATH_MAX (du reads it with openat; the portable backend
    // cannot, see the fixture).
    #[cfg(unix)]
    if let Some(du) = du(&root, &["--exclude=deep"]) {
        let ours_root = ours[&key(&root)].alloc - ours[&deep].alloc;
        assert_eq!(ours_root, du, "disk usage outside deep/ differs from du");
    }

    // The native backends (Linux, macOS) read the whole tree: the reference everywhere it can
    // read, and du for the total.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let native = scan(dfdu_scan::NativeBackend::default(), &root);
        let mut d = diffs(&native, &want, &deep);
        d.retain(|l| !l.starts_with(&format!("{}:", root.display())));
        assert!(d.is_empty(), "native backend: {d:#?}");
        let n_deep = &native[&deep];
        assert_eq!(
            (n_deep.items, n_deep.errors),
            (27, 0),
            "deep/: 25 levels and a file below it, all read"
        );
        let (n_root, w_root) = (&native[&key(&root)], &want[&key(&root)]);
        assert_eq!(
            n_root.items,
            w_root.items - want[&deep].items + n_deep.items
        );
        assert_eq!(n_root.errors, w_root.errors - want[&deep].errors);
        if let Some(du) = du(&root, &[]) {
            assert_eq!(
                n_root.alloc, du,
                "native backend: disk usage differs from du"
            );
        }
        // Stat'ing in inode order (spinning disks) changes the order, not the result.
        #[cfg(target_os = "linux")]
        {
            let hdd = scan(
                dfdu_scan::LinuxBackend::for_storage(dfdu_sys::Storage::Rotational),
                &root,
            );
            assert!(hdd == native, "inode order changed the totals");
        }
    }

    // On Windows both backends read past MAX_PATH: the reference everywhere, and the portable
    // backend's totals for every directory.
    #[cfg(windows)]
    {
        let n_deep = &ours[&deep];
        assert_eq!((n_deep.items, n_deep.errors), (27, 0), "portable backend");
        let native = scan(dfdu_scan::WindowsBackend::default(), &root);
        let d = diffs(&native, &want, b"");
        assert!(d.is_empty(), "Windows backend: {d:#?}");
        assert!(native == ours, "Windows and portable backends differ");
    }

    // So the temporary directory can be removed.
    lock(&root.join("locked"), false);
}
