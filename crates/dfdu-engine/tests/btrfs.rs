//! Scanning, measurement and deletion on btrfs, against the fixture of `scripts/btrfs-fixture.sh`
//! whose mount point is in `DFDU_BTRFS_DIR`. Skipped (passing) when the variable is not set. Cases
//! that need root run only when the tests run as root.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_scan::{JobOptions, NativeBackend};
use dfdu_tree::{DirState, NodeFlags, NodeRef};

mod common;
use common::{child, node};

/// Tests run one at a time: they add and remove `work-*` directories in the shared fixture,
/// which other tests' totals would see.
fn fixture() -> Option<(PathBuf, std::sync::MutexGuard<'static, ()>)> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let dir = std::env::var_os("DFDU_BTRFS_DIR").map(PathBuf::from)?;
    Some((
        dir,
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    ))
}

fn sh(args: &[&str]) {
    let st = Proc::new(args[0]).args(&args[1..]).status().expect(args[0]);
    assert!(st.success(), "{args:?}");
}

/// A fresh directory in the fixture for one test.
fn workdir(f: &Path, tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = f.join(format!(
        "work-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir(&d).unwrap();
    d
}

fn engine(journal: &Path) -> Engine<NativeBackend> {
    Engine::start(
        NativeBackend::default(),
        EngineOptions {
            journal_dir: Some(journal.to_path_buf()),
            check_invariants: true,
            workers: 2,
            ..Default::default()
        },
    )
}

fn scan(e: &Engine<NativeBackend>, root: &Path, cross_mounts: bool) {
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions {
            cross_mounts,
            ..Default::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
}

fn delete(e: &Engine<NativeBackend>, items: Vec<NodeRef>, mode: Mode) {
    e.send(Command::Delete { items, mode });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
}

fn notices(e: &Engine<NativeBackend>) -> Vec<String> {
    e.read(|m| m.status.notices.clone())
}

#[test]
fn subvolumes_are_scanned_and_mounts_are_not() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &f, false);
    e.read(|m| {
        let t = &m.tree;
        let root = t.roots().next().unwrap();
        for name in ["sub", "snap-ro", "snap-rw"] {
            let ix = child(t, root, name);
            assert_eq!(t.dir(ix).unwrap().state, DirState::Complete, "{name}");
            assert!(t.flags(ix).has(NodeFlags::SUBVOL), "{name}");
            assert!(!t.flags(ix).has(NodeFlags::MOUNT_ROOT), "{name}");
            assert!(t.total(ix).items > 1, "{name} was not read");
        }
        let ro = child(t, root, "snap-ro");
        assert!(t.flags(ro).has(NodeFlags::RO_SUBVOL));
        let rec = &m.subvols[&t.node_ref(ro)];
        let origin = &m.subvols[&t.node_ref(child(t, root, "sub"))];
        assert_eq!(rec.parent_uuid, Some(origin.uuid));
        let mnt = child(t, root, "mnt");
        assert_eq!(t.dir(mnt).unwrap().state, DirState::Skipped);
        assert!(t.flags(mnt).has(NodeFlags::MOUNT_ROOT));
        assert_eq!(t.total(root).mounts, 1);
        assert_eq!(t.total(root).ro_subvols, 1);
        // The nested subvolume is scanned too.
        let nested = child(t, child(t, root, "sub"), "nested");
        assert!(t.flags(nested).has(NodeFlags::SUBVOL));
        assert!(t.total(nested).items > 1);
    });
    e.shutdown(Duration::from_secs(1));
}

#[test]
fn second_mount_is_not_counted_twice() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &f, true);
    let crossing = e.read(|m| {
        let t = &m.tree;
        let root = t.roots().next().unwrap();
        // `mnt` shows `sub`, which the scan reaches without it.
        let mnt = child(t, root, "mnt");
        assert!(t.flags(mnt).has(NodeFlags::DUP_MOUNT));
        assert_eq!(t.total(mnt).items, 1, "descended into the second mount");
        assert!(t.total(child(t, root, "sub")).items > 1);
        t.total(root)
    });
    e.shutdown(Duration::from_secs(1));
    // The root totals are those of a scan that stays on the mount.
    let e = engine(j.path());
    scan(&e, &f, false);
    let staying = e.read(|m| m.tree.total(m.tree.roots().next().unwrap()));
    e.shutdown(Duration::from_secs(1));
    assert_eq!(
        (
            crossing.alloc,
            crossing.referenced,
            crossing.apparent,
            crossing.items
        ),
        (
            staying.alloc,
            staying.referenced,
            staying.apparent,
            staying.items
        )
    );
}

/// `btrfs subvolume list` and `btrfs qgroup show` output for the fixture's subvolumes, by path
/// relative to the fixture (its top level): id, uuid, parent uuid, read-only, and quota group
/// `(referenced, exclusive)`.
type ListedSubvol = (u64, String, Option<String>, bool, (u64, u64));

fn btrfs_subvolumes(f: &Path) -> HashMap<String, ListedSubvol> {
    let run = |args: &[&str]| {
        let out = Proc::new("btrfs").args(args).arg(f).output().unwrap();
        assert!(out.status.success(), "btrfs {args:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let read_only: Vec<String> = run(&["subvolume", "list", "-r"])
        .lines()
        .filter_map(|l| l.split(" path ").nth(1).map(str::to_string))
        .collect();
    let mut qgroups: HashMap<u64, (u64, u64)> = HashMap::new();
    for l in run(&["qgroup", "show", "--raw"]).lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if let [id, rfer, excl, ..] = w[..]
            && let Some(id) = id.strip_prefix("0/").and_then(|x| x.parse().ok())
        {
            qgroups.insert(id, (rfer.parse().unwrap(), excl.parse().unwrap()));
        }
    }
    let mut out = HashMap::new();
    for l in run(&["subvolume", "list", "-u", "-q"]).lines() {
        // ID 258 gen 9 top level 5 parent_uuid <uuid or -> uuid <uuid> path snap-ro
        let w: Vec<&str> = l.split_whitespace().collect();
        let at = |k: &str| w[w.iter().position(|x| *x == k).unwrap() + 1];
        let id: u64 = at("ID").parse().unwrap();
        let parent = Some(at("parent_uuid").to_string()).filter(|p| p != "-");
        let path = l.split(" path ").nth(1).unwrap().to_string();
        let ro = read_only.contains(&path);
        let q = qgroups.get(&id).copied().unwrap_or_default();
        out.insert(path, (id, at("uuid").to_string(), parent, ro, q));
    }
    out
}

/// `b` in lowercase hex.
fn hex(b: &[u8]) -> String {
    b.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn uuid(u: &[u8; 16]) -> String {
    let h = hex(u);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// As root (`btrfs subvolume list` and quota groups need it): every subvolume's
/// record matches btrfs's list, and its quota group numbers match `btrfs qgroup show`.
#[test]
fn subvolume_records_and_quota_groups_match_btrfs() {
    let Some((f, _lock)) = fixture() else { return };
    if !dfdu_sys::is_privileged() {
        return;
    }
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &f, false);
    let want = btrfs_subvolumes(&f);
    let mut paths: Vec<&String> = want.keys().collect();
    paths.sort();
    assert_eq!(paths, ["snap-ro", "snap-rw", "sub", "sub/nested"]);
    for (path, (id, uid, parent, ro, q)) in &want {
        let r = node(&e, path);
        let got = e.read(|m| {
            let ix = m.tree.resolve(r).unwrap();
            assert_eq!(
                m.tree.flags(ix).has(NodeFlags::RO_SUBVOL),
                *ro,
                "{path} read-only flag"
            );
            m.subvols[&r].clone()
        });
        assert_eq!(
            (
                got.id,
                uuid(&got.uuid),
                got.parent_uuid.as_ref().map(uuid),
                got.read_only,
                got.qgroup
            ),
            (*id, uid.clone(), parent.clone(), *ro, Some(*q)),
            "{path}"
        );
    }
    e.shutdown(Duration::from_secs(1));
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "fixture, work directory, journal and engine named alike in every test here"
)]
fn subvolumes_and_snapshots_are_deleted() {
    let Some((f, _lock)) = fixture() else { return };
    let w = workdir(&f, "del");
    let sv = w.join("sv");
    sh(&["btrfs", "-q", "subvolume", "create", sv.to_str().unwrap()]);
    fs::write(sv.join("data"), vec![1u8; 100_000]).unwrap();
    sh(&[
        "btrfs",
        "-q",
        "subvolume",
        "snapshot",
        sv.to_str().unwrap(),
        w.join("snap").to_str().unwrap(),
    ]);
    fs::create_dir(w.join("d")).unwrap();
    sh(&[
        "btrfs",
        "-q",
        "subvolume",
        "create",
        w.join("d/inner").to_str().unwrap(),
    ]);
    fs::write(w.join("d/inner/x"), b"x").unwrap();

    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &w, false);
    let uuid_of = |p: &Path| hex(&dfdu_sys::subvol_info(p).unwrap().unwrap().uuid);
    // Each item's intent record names the subvolume it is in: the snapshot and the subvolume
    // are their own, `d` is in the fixture's top level.
    let want = [
        ("snap", uuid_of(&w.join("snap"))),
        ("d", uuid_of(&f)),
        ("sv", uuid_of(&sv)),
    ];
    delete(
        &e,
        vec![node(&e, "snap"), node(&e, "d"), node(&e, "sv")],
        Mode::Permanent,
    );
    let journal: String = fs::read_dir(j.path())
        .unwrap()
        .map(|f| fs::read_to_string(f.unwrap().path()).unwrap())
        .collect();
    for (name, uuid) in want {
        let path = format!("\"path\":\"{}\"", w.join(name).display());
        let intent = journal
            .lines()
            .find(|l| l.contains("\"t\":\"intent\"") && l.contains(&path))
            .unwrap_or_else(|| panic!("no intent for {name}: {journal}"));
        assert!(
            intent.contains(&format!("\"subvol_uuid\":\"{uuid}\"")),
            "{intent}"
        );
    }
    let s = e.read(|m| m.status.last_delete.clone()).expect("summary");
    assert_eq!(
        (s.deleted, s.failed, s.refused),
        (3, 0, 0),
        "{:?}",
        notices(&e)
    );
    for p in ["snap", "d", "sv"] {
        assert!(!w.join(p).exists(), "{p} still exists");
    }
    assert!(
        notices(&e)
            .iter()
            .any(|n| n.contains("btrfs frees the space"))
    );
    e.shutdown(Duration::from_secs(1));
    let _ = fs::remove_dir(&w);
}

#[test]
fn read_only_snapshots() {
    let Some((f, _lock)) = fixture() else { return };
    let w = workdir(&f, "ro");
    let sv = w.join("sv");
    sh(&["btrfs", "-q", "subvolume", "create", sv.to_str().unwrap()]);
    fs::write(sv.join("data"), b"d").unwrap();
    sh(&[
        "btrfs",
        "-q",
        "subvolume",
        "snapshot",
        "-r",
        sv.to_str().unwrap(),
        w.join("ro").to_str().unwrap(),
    ]);
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &w, false);
    // Inside it: never.
    delete(&e, vec![node(&e, "ro/data")], Mode::Permanent);
    assert!(w.join("ro/data").exists());
    assert!(
        notices(&e).iter().any(|n| n.contains("read-only snapshot")),
        "{:?}",
        notices(&e)
    );
    // The snapshot itself: root deletes it, others are refused before anything is touched.
    delete(&e, vec![node(&e, "ro")], Mode::Permanent);
    if dfdu_sys::is_privileged() {
        assert!(!w.join("ro").exists());
    } else {
        assert!(w.join("ro/data").exists());
        assert!(
            notices(&e).iter().any(|n| n.contains("needs root")),
            "{:?}",
            notices(&e)
        );
    }
    e.shutdown(Duration::from_secs(1));
    // Clean up: the owner may make the snapshot writable, then delete it.
    if w.join("ro").exists() {
        sh(&[
            "btrfs",
            "property",
            "set",
            w.join("ro").to_str().unwrap(),
            "ro",
            "false",
        ]);
        dfdu_clean::remove(&w.join("ro"), dfdu_tree::Kind::Dir).unwrap();
    }
    dfdu_clean::remove(&w, dfdu_tree::Kind::Dir).unwrap();
}

#[test]
fn a_mount_inside_an_item_is_refused() {
    let Some((f, _lock)) = fixture() else { return };
    if !dfdu_sys::is_privileged() {
        return;
    }
    let w = workdir(&f, "mount");
    fs::create_dir_all(w.join("h/mp")).unwrap();
    fs::write(w.join("h/file"), b"x").unwrap();
    sh(&[
        "mount",
        "-t",
        "tmpfs",
        "tmpfs",
        w.join("h/mp").to_str().unwrap(),
    ]);
    fs::write(w.join("h/mp/precious"), b"p").unwrap();
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &w, false);
    delete(&e, vec![node(&e, "h")], Mode::Permanent);
    let refused = notices(&e)
        .iter()
        .any(|n| n.contains("contains 1 mount point"));
    let intact = w.join("h/file").exists() && w.join("h/mp/precious").exists();
    e.shutdown(Duration::from_secs(1));
    sh(&["umount", w.join("h/mp").to_str().unwrap()]);
    let _ = fs::remove_dir_all(&w);
    assert!(refused && intact, "refused {refused}, intact {intact}");
}

/// Root only (mounts a tmpfs): a mount that appears after the scan stops the delete at the
/// mount; nothing on the mounted filesystem is touched.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "fixture, work directory, journal and engine named alike in every test here"
)]
fn a_mount_made_after_the_scan_stops_the_delete() {
    let Some((f, _lock)) = fixture() else { return };
    if !dfdu_sys::is_privileged() {
        return;
    }
    let w = workdir(&f, "late-mount");
    fs::create_dir_all(w.join("h/mp")).unwrap();
    fs::write(w.join("h/file"), b"x").unwrap();
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &w, false);
    let mp = w.join("h/mp");
    sh(&["mount", "-t", "tmpfs", "tmpfs", mp.to_str().unwrap()]);
    fs::write(mp.join("precious"), b"p").unwrap();
    delete(&e, vec![node(&e, "h")], Mode::Permanent);
    let s = e.read(|m| m.status.last_delete.clone()).expect("summary");
    let n = notices(&e);
    let intact = mp.join("precious").exists();
    e.shutdown(Duration::from_secs(1));
    sh(&["umount", mp.to_str().unwrap()]);
    let _ = fs::remove_dir_all(&w);
    assert!(intact, "deleted into the mount");
    assert_eq!(s.deleted, 0, "{n:?}");
    // Either check stops it: the listing of `h` taken by the scan does not match what is
    // inside now (the mount's files), or the mount is found when deleting.
    assert!(
        n.iter()
            .any(|t| t.contains("a filesystem is mounted at")
                || t.contains("mp changed since the scan")),
        "{n:?}"
    );
}

/// Brute-force reference for shared-data accounting: every file's extent map, every byte range
/// (or, in exact mode, every shared extent) counted at the lowest common ancestor of the files
/// referencing it. Exact (extent tree) when running as root, FIEMAP otherwise, like the engine.
/// Also returns the directories' own allocation, which `btrfs filesystem du` leaves out.
fn reference(root: &Path) -> (HashMap<PathBuf, u64>, u64) {
    use std::os::unix::fs::MetadataExt;
    let root_mount = dfdu_sys::mount_id(root).unwrap();
    let mut own: HashMap<PathBuf, u64> = HashMap::new();
    let mut dir_blocks = fs::metadata(root).unwrap().blocks() * 512;
    own.insert(root.to_path_buf(), dir_blocks);
    let mut files: Vec<(PathBuf, dfdu_engine::Measurement)> = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        own.entry(d.clone()).or_default();
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                *own.entry(p.clone()).or_default() += m.blocks() * 512;
                dir_blocks += m.blocks() * 512;
                if dfdu_sys::mount_id(&p).unwrap() == root_mount {
                    stack.push(p);
                }
            } else if m.is_file() && m.blocks() > 0 {
                let f = fs::File::open(&p).unwrap();
                let ext = dfdu_sys::fiemap(&f).unwrap();
                let fsid = dfdu_sys::btrfs_fsid(&f).unwrap();
                let me = if dfdu_sys::is_privileged() {
                    let items = dfdu_sys::file_extents(&f, m.ino()).unwrap();
                    dfdu_engine::measure_from_items(fsid, &items, &ext)
                } else {
                    dfdu_engine::measure_from_extents(fsid, &ext)
                };
                files.push((p, me));
            } else {
                own.entry(p).or_default();
            }
        }
    }
    let lca = |paths: &[&PathBuf]| -> PathBuf {
        let mut l = paths[0].clone();
        for p in &paths[1..] {
            while !p.starts_with(&l) {
                l.pop();
            }
        }
        l
    };
    // Elementary intervals of the shared ranges.
    let mut bounds: Vec<u64> = files
        .iter()
        .flat_map(|(_, m)| m.ranges.iter().flat_map(|&(a, b)| [a, b]))
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        let refs: Vec<&PathBuf> = files
            .iter()
            .filter(|(_, m)| m.ranges.iter().any(|&(x, y)| x <= a && b <= y))
            .map(|(p, _)| p)
            .collect();
        if !refs.is_empty() {
            *own.entry(lca(&refs)).or_default() += b - a;
        }
    }
    let mut points: HashMap<u64, (u64, Vec<&PathBuf>)> = HashMap::new();
    for (p, m) in &files {
        for &(at, len) in &m.points {
            let e = points.entry(at).or_default();
            e.0 = e.0.max(len);
            e.1.push(p);
        }
    }
    for (len, refs) in points.values() {
        *own.entry(lca(refs)).or_default() += len;
    }
    for (p, m) in &files {
        *own.entry(p.clone()).or_default() += m.excl;
    }
    // Totals: each node's own bytes added to it and every ancestor inside the root.
    let mut total: HashMap<PathBuf, u64> = HashMap::new();
    for (p, b) in &own {
        let mut cur = p.clone();
        loop {
            *total.entry(cur.clone()).or_default() += b;
            if cur == root || !cur.pop() {
                break;
            }
        }
    }
    (total, dir_blocks)
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "fixture, work directory, journal and engine named alike in every test here"
)]
fn shared_data_matches_the_reference_and_btrfs() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &f, false);
    let (want, dir_blocks) = reference(&f);
    let got: HashMap<PathBuf, u64> = e.read(|m| {
        let mut out = HashMap::new();
        let mut stack = vec![m.tree.roots().next().unwrap()];
        while let Some(ix) = stack.pop() {
            out.insert(m.tree.path_buf(ix), m.tree.total(ix).alloc);
            stack.extend(m.tree.children(ix));
        }
        out
    });
    let mut diffs: Vec<String> = want
        .iter()
        .filter(|(p, w)| got.get(*p) != Some(w))
        .map(|(p, w)| format!("{}: got {:?} want {w}", p.display(), got.get(p)))
        .collect();
    diffs.sort();
    assert!(diffs.is_empty() && got.len() == want.len(), "{diffs:#?}");

    if dfdu_sys::is_privileged() {
        // Exact: the root total is what compsize reports as disk usage (every extent once, at
        // its size on disk; compressed, inline and partly referenced extents included).
        let out = Proc::new("compsize")
            .args(["-b"])
            .arg(&f)
            .output()
            .expect("compsize");
        let text = String::from_utf8_lossy(&out.stdout);
        let total: u64 = text
            .lines()
            .find(|l| l.starts_with("TOTAL"))
            .and_then(|l| l.split_whitespace().nth(2))
            .and_then(|x| x.parse().ok())
            .unwrap_or_else(|| panic!("compsize: {text}"));
        assert_eq!(got[&f] - dir_blocks, total, "compsize: {text}");
        e.read(|m| {
            assert!(m.status.exact_sizes && !m.status.compressed_upper_bound);
            let sub = child(&m.tree, m.tree.roots().next().unwrap(), "sub");
            let q = m.subvols[&m.tree.node_ref(sub)]
                .qgroup
                .expect("quota group");
            assert!(q.0 >= 10 << 20, "{q:?}");
            assert_eq!(m.status.profile, Some(("single", 1.0)));
        });
        e.shutdown(Duration::from_secs(1));
        return;
    }
    // The root total is what btrfs reports as exclusive plus shared data.
    let out = Proc::new("btrfs")
        .args(["filesystem", "du", "-s", "--raw"])
        .arg(&f)
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let n: Vec<u64> = line
        .split_whitespace()
        .take(3)
        .map(|x| x.parse().unwrap())
        .collect();
    assert_eq!(
        got[&f] - dir_blocks,
        n[1] + n[2],
        "btrfs filesystem du: {line}"
    );
    // Referenced sizes are du's (st_blocks, inline files as a block). Run inside the fixture:
    // its mount point is itself named `mnt`, which the exclusion would also match.
    let du = Proc::new("du")
        .args(["-s", "-B1", "--exclude=./mnt", "."])
        .current_dir(&f)
        .output()
        .unwrap();
    let du: u64 = String::from_utf8_lossy(&du.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    e.read(|m| {
        let t = &m.tree;
        let root = t.roots().next().unwrap();
        assert_eq!(t.total(root).referenced, du, "referenced is what du counts");
        assert_eq!(
            t.total(root).shared_out,
            0,
            "everything shared is inside the scan"
        );
    });
    e.shutdown(Duration::from_secs(1));
}

#[test]
fn a_snapshot_alone_reports_its_shared_data_as_shared_outside() {
    let Some((f, _lock)) = fixture() else { return };
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &f.join("snap-rw"), false);
    e.read(|m| {
        let t = &m.tree.total(m.tree.roots().next().unwrap());
        // a.bin (8 MiB) and d/b.bin (2 MiB) are shared with sub and snap-ro.
        assert_eq!(t.shared_out, 10 << 20);
        assert_eq!(t.alloc, 11 << 20);
    });
    e.shutdown(Duration::from_secs(1));
}

const TRASH_INNER: &str = "DFDU_BTRFS_TRASH_INNER";

/// With the trash in the top-level subvolume, an item in another subvolume is refused for
/// trash (a rename between subvolumes is impossible, so it would be copied). Re-runs this binary
/// with `XDG_DATA_HOME` set, since changing the environment of a running process needs `unsafe`.
#[test]
fn trash_refuses_items_in_another_subvolume() {
    let Some((f, _lock)) = fixture() else { return };
    if std::env::var_os(TRASH_INNER).is_some() {
        return;
    }
    let w = workdir(&f, "trash");
    let sv = w.join("sv");
    sh(&["btrfs", "-q", "subvolume", "create", sv.to_str().unwrap()]);
    fs::write(sv.join("x"), b"x").unwrap();
    fs::create_dir(w.join("data")).unwrap();
    let out = Proc::new(std::env::current_exe().unwrap())
        .args(["trash_inner", "--exact", "--nocapture"])
        .env(TRASH_INNER, &w)
        .env("XDG_DATA_HOME", w.join("data"))
        .output()
        .unwrap();
    let ok = out.status.success();
    dfdu_clean::remove(&w, dfdu_tree::Kind::Dir).unwrap();
    assert!(
        ok,
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn trash_inner() {
    let Some(w) = std::env::var_os(TRASH_INNER).map(PathBuf::from) else {
        return;
    };
    let j = tempfile::tempdir().unwrap();
    let e = engine(j.path());
    scan(&e, &w, false);
    delete(&e, vec![node(&e, "sv/x")], Mode::Trash);
    let n = notices(&e);
    assert!(w.join("sv/x").exists(), "{n:?}");
    // The message names the item's subvolume.
    let named = format!(
        "in btrfs subvolume {}, the trash in another",
        w.join("sv").display()
    );
    assert!(n.iter().any(|t| t.contains(&named)), "{n:?}");
    e.shutdown(Duration::from_secs(1));
}
