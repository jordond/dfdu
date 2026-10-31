//! Crash consistency: `dfdu` is killed with SIGKILL at
//! random points while it writes a snapshot, while it records a scan in the history, and while
//! `dfdu clean` deletes (permanently, staged with the commit that follows, and on Linux to the
//! trash). After each kill the previous snapshot still loads, the history opens, and every
//! item the kill left half done is reported by `dfdu recover`, which can finish or dismiss it.
//!
//! Rounds per case: `DFDU_CRASH_ROUNDS` (default 6). The seed is printed; `DFDU_CRASH_SEED`
//! repeats a run.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::ok;

const PROJECTS: usize = 12;

/// The cases time the runs they kill, so they run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
const PACKAGES: usize = 150;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// A duration in `[lo, hi)`.
    fn between(&mut self, lo: Duration, hi: Duration) -> Duration {
        let span = u64::try_from(hi.checked_sub(lo).unwrap().as_micros().max(1)).unwrap();
        lo + Duration::from_micros(self.next() % span)
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a seed: the low bits of the clock are enough"
)]
fn rng() -> Rng {
    let seed = std::env::var("DFDU_CRASH_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                | 1
        });
    eprintln!("DFDU_CRASH_SEED={seed}");
    Rng(seed)
}

fn rounds() -> usize {
    std::env::var("DFDU_CRASH_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
}

/// The `dfdu` binary with its state and its trash (`XDG_DATA_HOME`) below `state`.
fn sandboxed(state: &Path) -> Command {
    let mut c = common::bin_in(state);
    c.env("XDG_DATA_HOME", state.join("data"));
    c
}

fn run(state: &Path, args: &[&str]) -> Output {
    sandboxed(state).args(args).output().unwrap()
}

/// Starts `dfdu args`, kills it after `after` (if it is still running), and says whether it
/// was killed.
fn kill_after(state: &Path, args: &[&str], after: Duration) -> bool {
    let mut child = sandboxed(state)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while t0.elapsed() < after {
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    let _ = child.kill();
    let status = child.wait().unwrap();
    !status.success()
}

/// How long `dfdu args` takes when left alone: the shortest of three runs, each after
/// `reset`.
fn timed(state: &Path, args: &[&str], reset: &dyn Fn()) -> Duration {
    (0..3)
        .map(|_| {
            reset();
            let t0 = Instant::now();
            ok(run(state, args));
            t0.elapsed()
        })
        .min()
        .unwrap()
}

/// Projects, each with a lockfile and `node_modules` of `PACKAGES` packages of one file.
fn tree(root: &Path) {
    for p in 0..PROJECTS {
        let dir = root.join(format!("p{p:02}"));
        for k in 0..PACKAGES {
            let pkg = dir.join(format!("node_modules/pkg{k:03}"));
            fs::create_dir_all(&pkg).unwrap();
            fs::write(pkg.join("index.js"), vec![b'x'; 4096]).unwrap();
        }
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("package-lock.json"), "{}").unwrap();
    }
}

fn base() -> tempfile::TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

#[test]
fn killed_while_writing_a_snapshot() {
    let _serial = serial();
    let tmp = base();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    tree(&root);
    let snap = tmp.path().join("scan.snap");
    let r = root.to_str().unwrap();
    let s = snap.to_str().unwrap();
    let args = ["export", r, "--snapshot", "-o", s];
    let full = timed(&state, &args, &|| {});
    let total = ok(run(&state, &["import", s, "--bytes"]));
    let mut rng = rng();
    let mut killed = 0;
    for _ in 0..rounds() * 2 {
        killed += usize::from(kill_after(
            &state,
            &args,
            rng.between(full / 3, full * 9 / 10),
        ));
        // The previous snapshot, whole: a write is a rename of a complete file.
        assert_eq!(ok(run(&state, &["import", s, "--bytes"])), total);
    }
    eprintln!("snapshot: {killed} runs killed");
    assert!(killed > 0, "no run was killed");
    // The temporary files of killed writers go with the next write.
    ok(run(&state, &args));
    let left: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .filter(|n| n.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn killed_while_recording_history() {
    let _serial = serial();
    let tmp = base();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    tree(&root);
    let r = root.to_str().unwrap();
    let full = timed(&state, &["scan", r], &|| {});
    let count = || ok(run(&state, &["history"])).lines().count();
    let mut rng = rng();
    let mut killed = 0;
    for _ in 0..rounds() * 2 {
        let before = count();
        killed += usize::from(kill_after(
            &state,
            &["scan", r],
            rng.between(full / 3, full * 9 / 10),
        ));
        let after = count();
        assert!(
            after == before || after == before + 1,
            "{before} -> {after}"
        );
    }
    eprintln!("history: {killed} runs killed");
    assert!(killed > 0, "no run was killed");
    // Still writable.
    let before = count();
    ok(run(&state, &["scan", r]));
    assert_eq!(count(), before + 1);
}

/// The files left in a project's `node_modules`, or `None` when it is gone.
fn left(root: &Path, p: usize) -> Option<usize> {
    let nm = root.join(format!("p{p:02}/node_modules"));
    fs::symlink_metadata(&nm).ok()?;
    let mut n = 0;
    let mut stack = vec![nm];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                n += 1;
            }
        }
    }
    Some(n)
}

/// Items staged in `stage` (the top-level entries of each session's directory).
fn staged(state: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for s in fs::read_dir(state.join("stage"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for e in fs::read_dir(s.path()).into_iter().flatten().flatten() {
            out.push(e.path());
        }
    }
    out
}

/// After a killed `clean`: whatever the kill left half done is in `dfdu recover`'s list, and
/// finishing or dismissing every item leaves nothing unfinished and nothing staged.
fn check_and_recover(state: &Path, root: &Path, case: &str) -> usize {
    let list = ok(run(state, &["recover"]));
    let lines: Vec<&str> = list.lines().filter(|l| l.starts_with(' ')).collect();
    let listed = |p: &Path| {
        let p = format!("{}:", p.display());
        lines.iter().find(|l| l.contains(&p)).copied()
    };
    for p in 0..PROJECTS {
        assert!(
            root.join(format!("p{p:02}/package.json")).exists(),
            "{case}"
        );
        let nm = root.join(format!("p{p:02}/node_modules"));
        if let Some(n) = left(root, p)
            && n < PACKAGES
        {
            // Part of it is gone: the journal must say so.
            let l = listed(&nm).unwrap_or_else(|| {
                panic!(
                    "{case}: {} has {n} files and is not listed:\n{list}",
                    nm.display()
                )
            });
            assert!(l.contains("interrupted"), "{case}: {l}");
        }
    }
    for s in staged(state) {
        let at = format!("{}", s.display());
        assert!(
            lines.iter().any(|l| l.contains(&at)),
            "{case}: {at} is staged and not listed:\n{list}"
        );
    }
    // Finish what is staged, dismiss what was interrupted.
    let mut finish = vec![];
    let mut dismiss = vec![];
    for l in &lines {
        let (n, rest) = l.trim_start().split_once("  ").unwrap();
        if rest.contains(": staged at") || rest.contains(": partly deleted") {
            finish.push(n.to_string());
        } else {
            dismiss.push(n.to_string());
        }
    }
    let mut args = vec!["recover"];
    let (f, d) = (finish.join(","), dismiss.join(","));
    if !finish.is_empty() {
        args.extend(["--finish", &f, "--yes"]);
    }
    if !dismiss.is_empty() {
        args.extend(["--dismiss", &d]);
    }
    if args.len() > 1 {
        ok(run(state, &args));
    }
    assert_eq!(
        ok(run(state, &["recover"])).trim(),
        "nothing unfinished",
        "{case}"
    );
    assert!(staged(state).is_empty(), "{case}: {:?}", staged(state));
    lines.len()
}

fn killed_while_cleaning(mode: &str) {
    let _serial = serial();
    let tmp = base();
    let (root, state) = (tmp.path().join("tree"), tmp.path().join("state"));
    let r = root.to_str().unwrap();
    let args = [
        "clean",
        r,
        "--yes",
        "--all",
        "--safety",
        "review",
        "--mode",
        mode,
        "--threads",
        "2",
    ];
    let reset = || {
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        tree(&root);
    };
    let full = timed(&state, &args, &reset);
    assert!(
        (0..PROJECTS).all(|p| left(&root, p).is_none()),
        "{mode}: not all deleted"
    );
    let mut rng = rng();
    let (mut killed, mut reported) = (0, 0);
    for round in 0..rounds() {
        reset();
        killed += usize::from(kill_after(
            &state,
            &args,
            rng.between(full / 4, full * 9 / 10),
        ));
        reported += check_and_recover(&state, &root, &format!("{mode} round {round}"));
    }
    eprintln!("{mode}: {killed} runs killed, {reported} items reported");
    assert!(killed > 0, "no run was killed");
}

#[test]
fn killed_while_deleting() {
    killed_while_cleaning("delete");
}

#[test]
fn killed_while_staging_and_committing() {
    killed_while_cleaning("stage");
}

#[cfg(target_os = "linux")]
#[test]
fn killed_while_moving_to_the_trash() {
    killed_while_cleaning("trash");
}
