//! Native cleanup commands: the owning tool's own command (`cargo clean`,
//! `docker system prune`), run with no shell, stdin closed, a time limit and never with sudo.
//! The tool is checked first (`native.check`); a dry run or inspect step runs on its own
//! request, before the user confirms the real one. Output is sent line by line.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};

use crate::DeleterMsg;

/// How long the tool check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeReq {
    pub id: u64,
    /// Checked to exist and answer first; empty skips the check.
    pub check: Vec<String>,
    /// Run before `command`, in order (stopping a daemon that holds the files).
    pub pre: Vec<Vec<String>>,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    /// The dry run or inspect step: not journalled, and `pre` is not run.
    pub dry_run: bool,
    /// The rule it was built from, so that an agent on another host can build it again from
    /// its own copy of the rules instead of running a command it is sent.
    pub rule: Option<NativeRule>,
}

/// A native command as a rule and the matched path: what it is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeRule {
    pub rule: dfdu_rules::RuleIx,
    /// Index into the rule's `native.variant` list; `None` for its own command.
    pub variant: Option<u16>,
    pub path: PathBuf,
}

/// Shows an argv as a person would type it.
#[must_use]
pub fn shown(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(|c| c.is_whitespace() || "'\"$\\".contains(c)) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs `req`, sending its output and then [`DeleterMsg::NativeDone`]. `abort` stops it.
pub(crate) fn run(
    req: &NativeReq,
    abort: &AtomicBool,
    out: &Sender<DeleterMsg>,
) -> Result<(), String> {
    let line = |l: String| {
        let _ = out.send(DeleterMsg::NativeOutput {
            id: req.id,
            line: l,
        });
    };
    if !req.check.is_empty() {
        let (tx, _rx) = unbounded();
        run_one(&req.check, None, CHECK_TIMEOUT, abort, &tx).map_err(|e| {
            format!(
                "{} is not available: {e}",
                req.check.first().map_or("", |s| s.as_str())
            )
        })?;
    }
    let mut steps: Vec<&Vec<String>> = vec![];
    if !req.dry_run {
        steps.extend(req.pre.iter());
    }
    steps.push(&req.command);
    for argv in steps {
        line(format!("$ {}", shown(argv)));
        let (tx, rx) = unbounded();
        let id = req.id;
        let out2 = out.clone();
        let fwd = thread::spawn(move || {
            for l in rx {
                let _ = out2.send(DeleterMsg::NativeOutput { id, line: l });
            }
        });
        let r = run_one(argv, req.cwd.as_deref(), req.timeout, abort, &tx);
        drop(tx);
        let _ = fwd.join();
        r?;
    }
    Ok(())
}

/// Runs one command; its output lines go to `lines`. `Err` says why it failed.
fn run_one(
    argv: &[String],
    cwd: Option<&std::path::Path>,
    timeout: Duration,
    abort: &AtomicBool,
    lines: &Sender<String>,
) -> Result<(), String> {
    let (prog, rest) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|r| {
        let tx = lines.clone();
        thread::spawn(move || {
            for l in BufReader::new(r).lines() {
                match l {
                    Ok(l) => {
                        let _ = tx.send(l);
                    }
                    Err(_) => break,
                }
            }
        })
    })
    .collect();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if abort.load(SeqCst) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("stopped".to_string());
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("took longer than {} s; stopped", timeout.as_secs()));
            }
            // Polled: std cannot wait for a child with a time limit.
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(e.to_string()),
        }
    };
    for r in readers {
        let _ = r.join();
    }
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(match status.code() {
            Some(c) => format!("exited with status {c}"),
            None => "killed by a signal".into(),
        })
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    fn req(command: &[&str], timeout: Duration) -> NativeReq {
        NativeReq {
            id: 1,
            check: vec!["true".into()],
            pre: vec![vec!["echo".into(), "pre".into()]],
            command: command
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            cwd: None,
            timeout,
            dry_run: false,
            rule: None,
        }
    }

    fn output(rx: &crossbeam_channel::Receiver<DeleterMsg>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|m| match m {
                DeleterMsg::NativeOutput { line, .. } => Some(line),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn runs_pre_then_command_and_streams_output() {
        let (tx, rx) = unbounded();
        let r = req(
            &["sh", "-c", "echo out; echo err >&2"],
            Duration::from_secs(10),
        );
        run(&r, &AtomicBool::new(false), &tx).unwrap();
        let mut lines = output(&rx);
        lines[3..].sort();
        assert_eq!(
            lines,
            [
                "$ echo pre",
                "pre",
                "$ sh -c 'echo out; echo err >&2'",
                "err",
                "out"
            ]
        );
        // A dry run skips `pre`.
        run(
            &NativeReq { dry_run: true, ..r },
            &AtomicBool::new(false),
            &tx,
        )
        .unwrap();
        assert_eq!(output(&rx)[0], "$ sh -c 'echo out; echo err >&2'");
    }

    #[test]
    fn failures_are_reported() {
        let (tx, _rx) = unbounded();
        let stop = AtomicBool::new(false);
        let e = run(
            &req(&["sh", "-c", "exit 3"], Duration::from_secs(10)),
            &stop,
            &tx,
        );
        assert_eq!(e.unwrap_err(), "exited with status 3");
        let e = run(
            &req(&["sleep", "10"], Duration::from_millis(100)),
            &stop,
            &tx,
        );
        assert!(e.unwrap_err().contains("took longer"));
        let missing = NativeReq {
            check: vec!["dfdu-no-such-tool".into()],
            ..req(&["true"], Duration::from_secs(1))
        };
        let e = run(&missing, &stop, &tx).unwrap_err();
        assert!(e.contains("dfdu-no-such-tool is not available"), "{e}");
    }
}
