//! The classifier pool: checks too slow for the
//! scan workers, run on matched nodes after their job ends. A rule's `guard.not_vcs_tracked`
//! asks whether version control tracks files in the match (a `target/` committed to git is
//! protected); `guard.not_running` whether a process it names runs. A native `size_command`
//! (what `docker system df` reports) runs only when asked for (`Command::NativeSizes`). Like
//! the deleter and the measurers, the pool never touches the tree: the applier sends it
//! requests and applies its answers, which name nodes by [`NodeRef`], so an answer for a node
//! freed since is dropped.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::Sender;
use dfdu_tree::{NodeRef, RuleIx};
use parking_lot::Mutex;

use crate::threads::spawn_pool;

/// How long one external command may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub enum CheckReq {
    /// The guards of the rule `rule` matched at `node`, whose path is `path`.
    Guards {
        node: NodeRef,
        rule: RuleIx,
        path: PathBuf,
        vcs: bool,
        processes: Vec<String>,
    },
    /// Runs a rule's `native.check`, then its `size_command`.
    NativeSize {
        rule: RuleIx,
        check: Vec<String>,
        command: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CheckMsg {
    Guards {
        node: NodeRef,
        rule: RuleIx,
        /// `None` when it could not be told (no `git`, an error).
        tracked: Option<bool>,
        /// A process the rule names that runs.
        running: Option<String>,
    },
    NativeSize {
        rule: RuleIx,
        result: Result<u64, String>,
    },
}

/// Starts `n` classifier threads; answers go to `out`, one batch per request batch. They exit
/// when the returned sender is dropped.
///
/// # Panics
///
/// If the system cannot create a thread, as [`std::thread::spawn`] does.
#[must_use]
pub fn spawn_classifiers(n: usize, out: &Sender<Vec<CheckMsg>>) -> Sender<Vec<CheckReq>> {
    spawn_pool(
        "dfdu-classify",
        n,
        out,
        || (),
        |(), batch: Vec<CheckReq>| check_batch(&batch),
    )
}

/// Answers a batch, reading the process list once for all of it.
pub fn check_batch(batch: &[CheckReq]) -> Vec<CheckMsg> {
    let wants_processes =
        |r: &CheckReq| matches!(r, CheckReq::Guards { processes, .. } if !processes.is_empty());
    let procs = batch
        .iter()
        .any(wants_processes)
        .then(process_names)
        .flatten();
    batch
        .iter()
        .map(|r| match r {
            CheckReq::Guards {
                node,
                rule,
                path,
                vcs,
                processes,
            } => CheckMsg::Guards {
                node: *node,
                rule: *rule,
                tracked: if *vcs { git_tracked(path) } else { Some(false) },
                running: procs.as_ref().and_then(|p| {
                    processes
                        .iter()
                        .find(|n| p.iter().any(|q| same_process(q, n)))
                        .cloned()
                }),
            },
            CheckReq::NativeSize {
                rule,
                check,
                command,
            } => CheckMsg::NativeSize {
                rule: *rule,
                result: native_size(check, command),
            },
        })
        .collect()
}

/// Runs `argv` with no shell, stdin closed and a time limit; its stdout, or why it failed.
fn run(argv: &[String], cwd: Option<&Path>) -> Result<Vec<u8>, String> {
    let (prog, prog_args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(prog_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let Some(mut out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{prog}: no output pipe"));
    };
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let waited = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => break Err(format!("no answer in {}s", COMMAND_TIMEOUT.as_secs())),
            Err(e) => break Err(e.to_string()),
        }
    };
    let status = match waited {
        Ok(s) => s,
        Err(e) => {
            // A timeout or a failed wait: the child is killed and reaped, and the reader ends
            // when the pipe closes.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{prog}: {e}"));
        }
    };
    let buf = reader.join().unwrap_or_default();
    if status.success() {
        Ok(buf)
    } else {
        Err(format!("{prog}: {status}"))
    }
}

/// Whether git tracks a file in the directory at `path` (or the file itself): read from the
/// index of the nearest repository above it (`gix-index`), so no command runs and nothing in
/// the repository's configuration is executed. `Some(false)` outside a repository, `None` when
/// its index cannot be read.
#[must_use]
pub fn git_tracked(path: &Path) -> Option<bool> {
    let parent = path.parent()?;
    let Some(root) = parent.ancestors().find(|d| d.join(".git").exists()) else {
        return Some(false);
    };
    let rel = gix_path::into_bstr(path.strip_prefix(root).ok()?).ok()?;
    let rel = gix_path::to_unix_separators_on_windows(rel);
    let index = index(root)?;
    let mut dir = rel.to_vec();
    dir.push(b'/');
    Some(
        index.entry_by_path(rel.as_ref()).is_some()
            || index
                .prefixed_entries(dir.as_slice().into())
                .is_some_and(|e| !e.is_empty()),
    )
}

/// The index of the repository whose work tree is `root` (`.git` a directory, or a file naming
/// one as in linked work trees and submodules), kept and read again only when its file
/// changed. Empty when the repository has none yet.
fn index(root: &Path) -> Option<Arc<gix_index::File>> {
    type Kept = (PathBuf, Option<(SystemTime, u64)>, Arc<gix_index::File>);
    static KEPT: Mutex<Option<HashMap<PathBuf, Kept>>> = Mutex::new(None);
    let stamp = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())))
    };
    if let Some((file, at, index)) = KEPT.lock().get_or_insert_with(HashMap::new).get(root)
        && stamp(file) == *at
    {
        return Some(index.clone());
    }
    let dot = root.join(".git");
    let git_dir = if dot.is_dir() {
        dot
    } else {
        gix_discover::path::from_gitdir_file(&dot).ok()?
    };
    // A `.git` that names no repository is not read as one with nothing tracked.
    gix_discover::is_git(&git_dir).ok()?;
    let file = git_dir.join("index");
    let at = stamp(&file);
    let index = gix_index::File::at_or_default(
        &file,
        gix_hash::Kind::Sha1,
        false,
        gix_index::decode::Options::default(),
    )
    .ok()?;
    let index = Arc::new(index);
    let mut kept = KEPT.lock();
    let kept = kept.get_or_insert_with(HashMap::new);
    if kept.len() >= 64 {
        kept.clear();
    }
    kept.insert(root.to_path_buf(), (file, at, index.clone()));
    Some(index)
}

/// The names of running processes: `ps -axo comm=` (the file name of each command), or
/// `tasklist` on Windows. `None` when they cannot be listed.
fn process_names() -> Option<Vec<String>> {
    if cfg!(windows) {
        let argv = ["tasklist", "/fo", "csv", "/nh"].map(String::from);
        let out = run(&argv, None).ok()?;
        Some(
            String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|l| l.split(',').next())
                .map(|n| n.trim_matches('"').trim_end_matches(".exe").to_string())
                .collect(),
        )
    } else {
        let argv = ["ps", "-axo", "comm="].map(String::from);
        let out = run(&argv, None).ok()?;
        Some(
            String::from_utf8_lossy(&out)
                .lines()
                .map(|l| l.trim().rsplit('/').next().unwrap_or("").to_string())
                .filter(|n| !n.is_empty())
                .collect(),
        )
    }
}

/// Whether the listed process name `listed` is `name`; Linux cuts names to 15 bytes.
fn same_process(listed: &str, name: &str) -> bool {
    listed.eq_ignore_ascii_case(name)
        || (listed.len() == 15 && name.len() > 15 && name.starts_with(listed))
}

/// Runs `check` (the tool is there and working), then `command`, and reads the reclaimable
/// size from its output ([`parse_reclaimable`]).
fn native_size(check: &[String], command: &[String]) -> Result<u64, String> {
    if !check.is_empty() {
        run(check, None)?;
    }
    let out = run(command, None)?;
    parse_reclaimable(&String::from_utf8_lossy(&out))
        .ok_or_else(|| "no size in the output".to_string())
}

/// The reclaimable bytes a `size_command` printed: a plain number of bytes, or JSON lines
/// with a `"Reclaimable"` field (`docker system df --format json`: `"1.2GB (50%)"`), summed.
#[must_use]
pub fn parse_reclaimable(out: &str) -> Option<u64> {
    let out = out.trim();
    if let Ok(n) = out.parse::<u64>() {
        return Some(n);
    }
    let mut total = None;
    for line in out.lines() {
        let Some(at) = line.find("\"Reclaimable\"") else {
            continue;
        };
        let rest = &line[at + "\"Reclaimable\"".len()..];
        let value = rest.split('"').nth(1)?;
        let size = value.split_whitespace().next()?;
        // Sizes past 16 EiB are not sizes.
        total = Some(total.unwrap_or(0u64).checked_add(parse_unit_size(size)?)?);
    }
    total
}

/// `1.2GB`, `512kB`, `0B` and the like, as docker prints them (decimal units).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a size parsed from digits is not negative, and `as` saturates past u64::MAX"
)]
fn parse_unit_size(s: &str) -> Option<u64> {
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_sizes() {
        let out = r#"{"Active":"1","Reclaimable":"1.2GB (50%)","Size":"2.4GB","TotalCount":"3","Type":"Images"}
{"Active":"0","Reclaimable":"512kB (100%)","Size":"512kB","TotalCount":"1","Type":"Containers"}
{"Active":"0","Reclaimable":"0B","Size":"0B","TotalCount":"0","Type":"Local Volumes"}"#;
        assert_eq!(parse_reclaimable(out), Some(1_200_512_000));
        assert_eq!(parse_reclaimable("12345\n"), Some(12345));
        assert_eq!(parse_reclaimable("nothing"), None);
        // Found by the `native` fuzz target: lines that add up past u64.
        let big = format!("\"Reclaimable\":\"8{}\"\n", "5".repeat(19));
        assert_eq!(parse_reclaimable(&big.repeat(2)), None);
    }

    #[test]
    fn git_tracking() {
        let have_git = Command::new("git").arg("--version").output().is_ok();
        if !have_git {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(repo)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        for d in ["tracked", "untracked"] {
            std::fs::create_dir(repo.join(d)).unwrap();
            std::fs::write(repo.join(d).join("f"), "x").unwrap();
        }
        git(&["add", "tracked"]);
        assert_eq!(git_tracked(&repo.join("tracked")), Some(true));
        assert_eq!(git_tracked(&repo.join("untracked")), Some(false));
        // A name is not a pattern: `b[1]` would match the tracked file `b1`.
        std::fs::write(repo.join("b1"), "x").unwrap();
        std::fs::create_dir(repo.join("b[1]")).unwrap();
        std::fs::write(repo.join("b[1]/f"), "x").unwrap();
        git(&["add", "b1"]);
        assert_eq!(git_tracked(&repo.join("b[1]")), Some(false));
        assert_eq!(git_tracked(&repo.join("b1")), Some(true));
        // The kept index is read again once git changes it.
        git(&["add", "untracked"]);
        assert_eq!(git_tracked(&repo.join("untracked")), Some(true));
        // A repository inside another answers for its own files.
        let inner = repo.join("vendor/lib");
        std::fs::create_dir_all(inner.join("target")).unwrap();
        std::fs::write(inner.join("target/f"), "x").unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&inner)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        assert_eq!(git_tracked(&inner.join("target")), Some(false));
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("target")).unwrap();
        assert_eq!(git_tracked(&outside.path().join("target")), Some(false));
    }

    #[test]
    fn running_processes() {
        let Some(names) = process_names() else {
            return;
        };
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_string_lossy();
        // `comm` may be cut to 15 or 16 characters on Linux.
        assert!(
            names
                .iter()
                .any(|n| me.starts_with(n.as_str()) && n.len() >= 8),
            "{me} not among {} processes",
            names.len()
        );
    }
}
