//! Generated plans against the deleter (ROADMAP M6): whatever a plan asks for, nothing outside
//! the scan root is deleted, nothing inside a filesystem mounted below an item, and no item that
//! is a protected path or below one. Each case builds a random tree with protected names in it
//! and symlinks out of the root, takes the identities a scan would record, then changes the tree
//! the way a race with the scan could (a directory moved out of the root and a symlink left in
//! its place, a directory replaced by a symlink to a protected one) and runs a plan drawn from
//! entries inside and outside the root, paths with `..`, the root, its parent and relative
//! paths, permanent and staged. The mount case needs a disk image (macOS) or root (Linux).

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use dfdu_clean::{
    DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal, is_protected,
    spawn_deleter,
};
use dfdu_tree::{Kind, Meta, Tree};
use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

const CASES: u64 = 48;

fn canonical_tempdir() -> tempfile::TempDir {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn identity(p: &Path) -> Option<Identity> {
    let m = Meta::from_metadata(&fs::symlink_metadata(p).ok()?);
    Some(Identity {
        dev: m.dev,
        ino: m.ino,
        kind: m.kind,
        apparent: m.apparent,
        mtime: m.mtime,
    })
}

/// Every entry under `dir` (not following symlinks), with its identity.
fn entries(dir: &Path, out: &mut BTreeMap<PathBuf, Identity>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Some(id) = identity(&p) else { continue };
        out.insert(p.clone(), id);
        if id.kind == Kind::Dir {
            entries(&p, out);
        }
    }
}

/// Names the generated trees use: plain ones, and protected ones (by name, so on every
/// platform).
const PLAIN: &[&str] = &["a", "b", "cache", "build", "x.log", "node_modules", "data"];
const PROTECTED: &[&str] = &[".git", ".env", "id_rsa", "key.pem", ".ssh", "lost+found"];

/// A random tree under `dir`: directories and files, some with protected names.
fn grow(rng: &mut ChaCha8Rng, dir: &Path, depth: u32) {
    fs::create_dir_all(dir).unwrap();
    for _ in 0..rng.random_range(1..5) {
        let base = if rng.random_bool(0.25) {
            PROTECTED[rng.random_range(0..PROTECTED.len())]
        } else {
            PLAIN[rng.random_range(0..PLAIN.len())]
        };
        let p = dir.join(base);
        if p.exists() {
            continue;
        }
        if depth < 3 && rng.random_bool(0.5) {
            grow(rng, &p, depth + 1);
        } else {
            fs::write(&p, vec![1u8; rng.random_range(0..64)]).unwrap();
        }
    }
}

struct Case {
    tmp: PathBuf,
    root: PathBuf,
    outside: PathBuf,
    /// Directories mounted below the root (none without a disk image or root).
    mounts: Vec<PathBuf>,
}

/// One plan item: a path and the identity it claims.
fn item(tree: &mut Tree, path: PathBuf, identity: Identity) -> PlanItem {
    let m = Meta::from_metadata(&fs::symlink_metadata("/").unwrap());
    let r = tree.new_root(b"/", &m, false);
    PlanItem {
        node: tree.node_ref(r),
        path,
        identity,
        bytes: 0,
        inside: Default::default(),
    }
}

fn run(rx: &Receiver<DeleterMsg>, h: &dfdu_clean::DeleterHandle, plan: Plan) -> Vec<DeleteResult> {
    let (id, mode, n) = (plan.id, plan.mode, plan.items.len());
    h.reqs.send(DeleterReq::Plan(plan)).unwrap();
    let mut results = vec![];
    let mut staged = 0;
    while let Ok(m) = rx.recv_timeout(Duration::from_secs(30)) {
        match m {
            DeleterMsg::Item { plan, result, .. } if plan == id => {
                staged += usize::from(result == DeleteResult::Deleted);
                results.push(result);
            }
            DeleterMsg::Done { plan, .. } if plan == id => break,
            _ => {}
        }
    }
    assert_eq!(results.len(), n, "every item answered");
    if mode == Mode::Stage && staged > 0 {
        h.reqs.send(DeleterReq::Commit).unwrap();
        let mut committed = 0;
        while committed < staged {
            match rx.recv_timeout(Duration::from_secs(30)) {
                // A commit that fails (a mount inside) leaves the item staged.
                Ok(DeleterMsg::Committed { .. }) => committed += 1,
                Ok(_) => {}
                Err(e) => panic!("staged items not committed: {e}"),
            }
        }
    }
    results
}

fn check_case(c: &Case, seed: u64, mode: Mode) -> Vec<DeleteResult> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let here = c.root.join(format!("c{seed}"));
    let away = c.outside.join(format!("c{seed}"));
    grow(&mut rng, &here, 0);
    grow(&mut rng, &away, 0);
    // Symlinks out of the root and to a protected directory inside it.
    symlink(&away, here.join("out")).unwrap();
    fs::create_dir_all(here.join("p/.git/objects")).unwrap();
    fs::write(here.join("p/.git/config"), b"[core]").unwrap();
    fs::write(here.join("p/.git/objects/o"), b"o").unwrap();

    // What the scan saw.
    let mut seen = BTreeMap::new();
    entries(&here, &mut seen);
    let mut outside_seen = BTreeMap::new();
    entries(&away, &mut outside_seen);

    // Races with the scan: a directory moved out of the root with a symlink in its place,
    // and one replaced by a symlink to the protected directory.
    let dirs: Vec<PathBuf> = seen
        .iter()
        .filter(|(p, id)| id.kind == Kind::Dir && !is_protected(p) && !p.ends_with("p"))
        .map(|(p, _)| p.clone())
        .collect();
    let mut moved = vec![];
    for (n, d) in dirs.iter().enumerate() {
        if !d.exists() || fs::symlink_metadata(d).unwrap().is_symlink() {
            continue;
        }
        match rng.random_range(0..6) {
            0 => {
                let to = c.outside.join(format!("moved{seed}-{n}"));
                fs::rename(d, &to).unwrap();
                symlink(&to, d).unwrap();
                moved.push(to);
            }
            1 => {
                let to = c.outside.join(format!("gone{seed}-{n}"));
                fs::rename(d, &to).unwrap();
                symlink(here.join("p/.git"), d).unwrap();
                moved.push(to);
            }
            _ => {}
        }
    }

    // Everything as it is now, before the plan runs.
    let mut before = BTreeMap::new();
    entries(&c.tmp, &mut before);

    let mut tree = Tree::new();
    let mut items = vec![];
    let seen_list: Vec<(&PathBuf, &Identity)> = seen.iter().collect();
    let outside_list: Vec<(&PathBuf, &Identity)> = outside_seen.iter().collect();
    for _ in 0..rng.random_range(4..16) {
        let pick_seen = |rng: &mut ChaCha8Rng| seen_list[rng.random_range(0..seen_list.len())];
        let pick_out = |rng: &mut ChaCha8Rng| {
            outside_list
                .get(rng.random_range(0..outside_list.len().max(1)))
                .copied()
        };
        let it = match rng.random_range(0..10) {
            // Entries the scan saw, some of them now behind a symlink.
            0..=3 => {
                let (p, id) = pick_seen(&mut rng);
                item(&mut tree, p.clone(), *id)
            }
            // Outside the root, as is and through `..` or the symlink out.
            4 => match pick_out(&mut rng) {
                Some((p, id)) => item(&mut tree, p.clone(), *id),
                None => continue,
            },
            5 => match pick_out(&mut rng) {
                Some((p, id)) => {
                    let rel = p.strip_prefix(&c.outside).unwrap();
                    item(&mut tree, c.root.join("..").join("outside").join(rel), *id)
                }
                None => continue,
            },
            6 => match pick_out(&mut rng) {
                Some((p, id)) => {
                    let rel = p.strip_prefix(&away).unwrap();
                    item(&mut tree, here.join("out").join(rel), *id)
                }
                None => continue,
            },
            // The root, its parent, the filesystem root, a relative path.
            7 => {
                let p = [
                    c.root.clone(),
                    c.tmp.clone(),
                    PathBuf::from("/"),
                    PathBuf::from(format!("c{seed}/a")),
                ][rng.random_range(0..4)]
                .clone();
                let id = identity(&p).or_else(|| identity(&here)).unwrap();
                item(&mut tree, p, id)
            }
            // A mount point, what contains one, and what is inside one.
            8 if !c.mounts.is_empty() => {
                let m = &c.mounts[rng.random_range(0..c.mounts.len())];
                let p = [m.clone(), m.parent().unwrap().to_path_buf(), m.join("keep")]
                    [rng.random_range(0..3)]
                .clone();
                match identity(&p) {
                    Some(id) => item(&mut tree, p, id),
                    None => continue,
                }
            }
            // Below the protected directory, by its real path.
            _ => {
                let p = here.join("p/.git/objects");
                item(&mut tree, p.clone(), identity(&p).unwrap())
            }
        };
        items.push(it);
    }

    let state = canonical_tempdir();
    let (tx, rx) = unbounded();
    let h = spawn_deleter(
        Some(state.path().join("journals")),
        Duration::from_secs(600),
        tx,
    );
    let plan = Plan {
        id: seed,
        mode,
        scan_root: c.root.clone(),
        items: items.clone(),
    };
    let results = run(&rx, &h, plan);
    drop(h);

    // Items deleted, and those whose delete failed part-way.
    let deleted: Vec<&PlanItem> = items
        .iter()
        .zip(&results)
        .filter(|(_, r)| matches!(r, DeleteResult::Deleted | DeleteResult::Failed { .. }))
        .map(|(i, _)| i)
        .collect();
    let ctx = || format!("seed {seed}, {mode:?}: {items:#?}\n{results:#?}");
    for i in &deleted {
        let p = &i.path;
        assert!(
            p.starts_with(&c.root)
                && p != &c.root
                && !p.components().any(|c| matches!(c, Component::ParentDir)),
            "deleted {} outside the root; {}",
            p.display(),
            ctx()
        );
        assert!(
            !is_protected(p),
            "deleted protected {}; {}",
            p.display(),
            ctx()
        );
    }
    // Nothing that was there is gone unless a deleted item held it, and never into a mount
    // from above it.
    for (p, id) in &before {
        if fs::symlink_metadata(p).is_ok() {
            continue;
        }
        let by = deleted.iter().find(|i| p.starts_with(&i.path));
        let Some(by) = by else {
            panic!(
                "{} ({:?}) gone, no item held it; {}",
                p.display(),
                id.kind,
                ctx()
            );
        };
        for m in &c.mounts {
            assert!(
                !(p.starts_with(m) && !by.path.starts_with(m)),
                "deleted {} in the mount at {} through {}; {}",
                p.display(),
                m.display(),
                by.path.display(),
                ctx()
            );
        }
    }
    for m in &moved {
        assert!(m.exists(), "{} gone", m.display());
    }
    results
}

/// The generator reached every kind of answer at least once.
fn assert_coverage(all: &[DeleteResult], mounts: bool) {
    let has = |f: &dyn Fn(&DeleteResult) -> bool| all.iter().any(f);
    let stale = |why: &'static str| move |r: &DeleteResult| *r == DeleteResult::Stale(why.into());
    assert!(has(&|r| *r == DeleteResult::Deleted), "nothing deleted");
    for r in [
        Refusal::Protected,
        Refusal::OutsideScanRoot,
        Refusal::ScanRoot,
    ] {
        assert!(
            has(&|x| *x == DeleteResult::Refused(r.clone())),
            "never {r:?}"
        );
    }
    assert!(
        has(&stale("a directory above it is now a symlink")),
        "no symlinked parent"
    );
    if mounts {
        assert!(has(&|r| matches!(
            r,
            DeleteResult::Refused(Refusal::MountPoint)
        )));
        assert!(has(&|r| matches!(
            r,
            DeleteResult::Refused(Refusal::ContainsMount(_))
        )));
    }
}

fn layout(tmp: &Path) -> Case {
    let root = tmp.join("root");
    let outside = tmp.join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    Case {
        tmp: tmp.to_path_buf(),
        root,
        outside,
        mounts: vec![],
    }
}

#[test]
fn generated_plans_stay_inside_the_root() {
    let tmp = canonical_tempdir();
    let c = layout(tmp.path());
    let mut all = vec![];
    for seed in 0..CASES {
        let mode = if seed % 2 == 0 {
            Mode::Permanent
        } else {
            Mode::Stage
        };
        all.extend(check_case(&c, seed, mode));
    }
    assert_coverage(&all, false);
}

/// A mounted disk image, detached on drop.
#[cfg(target_os = "macos")]
struct Mounted(PathBuf);

#[cfg(target_os = "macos")]
fn mount_at(dir: &Path, img: &Path) -> Option<Mounted> {
    let run = |args: &[&std::ffi::OsStr]| {
        std::process::Command::new("hdiutil")
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let ok = run(&[
        "create".as_ref(),
        "-size".as_ref(),
        "8m".as_ref(),
        "-fs".as_ref(),
        "HFS+".as_ref(),
        "-volname".as_ref(),
        "dfduplans".as_ref(),
        img.as_os_str(),
    ]) && run(&[
        "attach".as_ref(),
        "-nobrowse".as_ref(),
        "-mountpoint".as_ref(),
        dir.as_os_str(),
        img.as_os_str(),
    ]);
    ok.then(|| Mounted(dir.to_path_buf()))
}

#[cfg(target_os = "macos")]
impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .arg("detach")
            .arg("-force")
            .arg(&self.0)
            .output();
    }
}

#[cfg(target_os = "linux")]
struct Mounted(PathBuf);

#[cfg(target_os = "linux")]
fn mount_at(dir: &Path, _img: &Path) -> Option<Mounted> {
    if !dfdu_sys::is_privileged() {
        return None;
    }
    std::process::Command::new("mount")
        .args(["-t", "tmpfs", "tmpfs"])
        .arg(dir)
        .status()
        .is_ok_and(|s| s.success())
        .then(|| Mounted(dir.to_path_buf()))
}

#[cfg(target_os = "linux")]
impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = std::process::Command::new("umount").arg(&self.0).status();
    }
}

/// With a filesystem mounted below the root: plans with the mount point, the directory that
/// holds it and entries inside it. Skipped where nothing can be mounted.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn generated_plans_do_not_cross_mounts() {
    let tmp = canonical_tempdir();
    let mut c = layout(tmp.path());
    let img = canonical_tempdir();
    let mut held = vec![];
    let mut all = vec![];
    for n in 0..2 {
        let dir = c.root.join(format!("m{n}/vol"));
        fs::create_dir_all(&dir).unwrap();
        let Some(m) = mount_at(&dir, &img.path().join(format!("v{n}.dmg"))) else {
            eprintln!("skipped: cannot mount a filesystem here");
            return;
        };
        fs::write(dir.join("keep"), b"k").unwrap();
        fs::write(dir.parent().unwrap().join("beside"), b"b").unwrap();
        c.mounts.push(dir);
        held.push(m);
    }
    for seed in 0..CASES {
        let mode = if seed % 2 == 0 {
            Mode::Permanent
        } else {
            Mode::Stage
        };
        all.extend(check_case(&c, 1000 + seed, mode));
    }
    assert_coverage(&all, true);
    for m in &c.mounts {
        assert!(m.exists(), "{} gone", m.display());
    }
}
