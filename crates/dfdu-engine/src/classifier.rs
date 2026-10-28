//! The classifier pool (architecture §6, `data-pipeline.md` §4.4): checks too slow for the
//! scan workers, run on matched nodes after their job ends. A rule's `guard.not_vcs_tracked`
//! asks whether version control tracks files in the match (a `target/` committed to git is
//! protected); `guard.not_running` whether a process it names runs. A native `size_command`
//! (what `docker system df` reports) runs only when asked for (`Command::NativeSizes`). Like
//! the deleter and the measurers, the pool never touches the tree: the applier sends it
//! requests and applies its answers, which name nodes by [`NodeRef`], so an answer for a node
//! freed since is dropped (scenario 13 in `data-pipeline.md` §10.2).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};
use dfdu_tree::NodeRef;

/// How long one external command may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub enum CheckReq {
    /// The guards of the rule `rule` matched at `node`, whose path is `path`.
    Guards {
        node: NodeRef,
        rule: u16,
        path: PathBuf,
        vcs: bool,
        processes: Vec<String>,
    },
    /// Runs a rule's `native.check`, then its `size_command`.
    NativeSize {
        rule: u16,
        check: Vec<String>,
        command: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CheckMsg {
    Guards {
        node: NodeRef,
        rule: u16,
        /// `None` when it could not be told (no `git`, an error).
        tracked: Option<bool>,
        /// A process the rule names that runs.
        running: Option<String>,
    },
    NativeSize {
        rule: u16,
        result: Result<u64, String>,
    },
}

/// Starts `n` classifier threads; answers go to `out`, one batch per request batch. They exit
/// when the returned sender is dropped.
pub fn spawn_classifiers(n: usize, out: Sender<Vec<CheckMsg>>) -> Sender<Vec<CheckReq>> {
    let (tx, rx) = unbounded::<Vec<CheckReq>>();
    for i in 0..n.max(1) {
        let (rx, out) = (rx.clone(), out.clone());
        thread::Builder::new()
            .name(format!("dfdu-classify-{i}"))
            .spawn(move || {
                for batch in rx {
                    let _ = out.send(check_batch(&batch));
                }
            })
            .expect("spawn classifier");
    }
    tx
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
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let mut out = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{prog}: no answer in {}s",
                    COMMAND_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("{prog}: {e}")),
        }
    };
    let buf = reader.join().unwrap_or_default();
    if status.success() {
        Ok(buf)
    } else {
        Err(format!("{prog}: {status}"))
    }
}

/// Whether git tracks a file in the directory at `path`: `git ls-files --error-unmatch` in
/// its parent. `Some(false)` outside a repository. The repository's own configuration could
/// name a command for git to run (`core.fsmonitor`), so that is turned off.
pub fn git_tracked(path: &Path) -> Option<bool> {
    let (parent, name) = (path.parent()?, path.file_name()?);
    if !inside_git_repo(parent) {
        return Some(false);
    }
    let argv: Vec<String> = [
        "git",
        "-c",
        "core.fsmonitor=false",
        "ls-files",
        "--error-unmatch",
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([name.to_string_lossy().into_owned()])
    .collect();
    match run(&argv, Some(parent)) {
        Ok(_) => Some(true),
        // Exit status 1: nothing under it is tracked.
        Err(e) if e.contains("exit status: 1") => Some(false),
        Err(_) => None,
    }
}

/// A `.git` in `dir` or a directory above it.
fn inside_git_repo(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
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
        *total.get_or_insert(0) += parse_unit_size(size)?;
    }
    total
}

/// `1.2GB`, `512kB`, `0B` and the like, as docker prints them (decimal units).
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
