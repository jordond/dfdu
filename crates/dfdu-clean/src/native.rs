//! Native cleanup commands: the owning tool's own command (`cargo clean`,
//! `docker system prune`), run with no shell, stdin closed, a time limit and never with sudo.
//! The tool is checked first (`native.check`); a dry run or inspect step runs on its own
//! request, before the user confirms the real one. Output is sent line by line.

use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

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
    pub rule: dfdu_tree::RuleIx,
    /// Index into the rule's `native.variant` list; `None` for its own command.
    pub variant: Option<u16>,
    pub path: dfdu_tree::HostPath,
}

/// Shows an argv as a person would type it.
#[must_use]
pub fn shown_argv(argv: &[String]) -> String {
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
    let (id, out) = (req.id, out.clone());
    let line = move |line| {
        let _ = out.send(DeleterMsg::NativeOutput { id, line });
    };
    check_tool(&req.check, abort)?;
    let mut steps: Vec<&Vec<String>> = vec![];
    if !req.dry_run {
        steps.extend(req.pre.iter());
    }
    steps.push(&req.command);
    for argv in steps {
        line(format!("$ {}", shown_argv(argv)));
        let line = line.clone();
        run_command(
            argv,
            req.cwd.as_deref(),
            req.timeout,
            abort,
            true,
            move |r| read_lines(r, &line),
        )?;
    }
    Ok(())
}

/// Runs `check` (empty checks nothing), which says the tool is there and working; `Err` says
/// why it is not, naming it.
///
/// # Errors
///
/// When `check` fails as [`run_command`] does.
pub fn check_tool(check: &[String], abort: &AtomicBool) -> Result<(), String> {
    let Some(tool) = check.first() else {
        return Ok(());
    };
    run_command(check, None, CHECK_TIMEOUT, abort, false, |mut r| {
        io::copy(&mut r, &mut io::sink()).map(drop)
    })
    .map(drop)
    .map_err(|e| format!("{tool} is not available: {e}"))
}

/// Sends each line `r` gives to `send`, lossily decoded, until it ends.
fn read_lines(r: Box<dyn Read + Send>, send: &impl Fn(String)) -> io::Result<()> {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let l = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        send(String::from_utf8_lossy(l).into_owned());
    }
}

/// Runs `argv` with no shell, stdin closed, in `cwd` when given, for at most `timeout`; `abort`
/// stops it. Its stdout, and its stderr when `stderr` is set (otherwise discarded), are each
/// read by `read` on a thread of their own; what they read is returned, stdout's first. `Err`
/// says why it failed, naming the program.
///
/// # Errors
///
/// When the program cannot be started, takes too long, is stopped, exits unsuccessfully, or
/// its output cannot be read.
pub fn run_command<T: Send + 'static>(
    argv: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
    abort: &AtomicBool,
    stderr: bool,
    read: impl Fn(Box<dyn Read + Send>) -> io::Result<T> + Clone + Send + 'static,
) -> Result<Vec<T>, String> {
    let (prog, rest) = argv.split_first().ok_or("empty command")?;
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let pipes = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ];
    let mut readers = Vec::new();
    let mut started = Ok(());
    for pipe in pipes.into_iter().flatten() {
        let read = read.clone();
        match thread::Builder::new()
            .name("dfdu-native-out".into())
            .spawn(move || read(pipe))
        {
            Ok(t) => readers.push(t),
            Err(e) => {
                started = Err(format!("cannot start a thread: {e}"));
                break;
            }
        }
    }
    let deadline = Instant::now() + timeout;
    let status = started.and_then(|()| {
        loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) if abort.load(SeqCst) => break Err("stopped".to_string()),
                Ok(None) if Instant::now() >= deadline => {
                    break Err(format!(
                        "{prog}: took longer than {} s; stopped",
                        timeout.as_secs()
                    ));
                }
                // Polled: std cannot wait for a child with a time limit.
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(e) => break Err(format!("{prog}: {e}")),
            }
        }
    });
    if status.is_err() {
        // The readers end when the pipes close.
        let _ = child.kill();
        let _ = child.wait();
    }
    let read: Result<Vec<T>, String> = readers
        .into_iter()
        .map(|r| match r.join() {
            Ok(Ok(t)) => Ok(t),
            Ok(Err(e)) => Err(format!("{prog}: reading output: {e}")),
            Err(_) => Err(format!("{prog}: reading output: its thread panicked")),
        })
        .collect();
    let status = status?;
    if !status.success() {
        return Err(match status.code() {
            Some(c) => format!("{prog}: exited with status {c}"),
            None => format!("{prog}: killed by a signal"),
        });
    }
    read
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use crossbeam_channel::unbounded;

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
        assert_eq!(e.unwrap_err(), "sh: exited with status 3");
        let e = run(
            &req(&["sleep", "10"], Duration::from_millis(100)),
            &stop,
            &tx,
        );
        assert!(e.unwrap_err().starts_with("sleep: took longer than "));
        let missing = NativeReq {
            check: vec!["dfdu-no-such-tool".into()],
            ..req(&["true"], Duration::from_secs(1))
        };
        let e = run(&missing, &stop, &tx).unwrap_err();
        assert!(e.contains("dfdu-no-such-tool is not available"), "{e}");
    }

    #[test]
    fn a_read_error_or_a_panicking_reader_fails_the_command() {
        let stop = AtomicBool::new(false);
        let argv = ["true".to_string()];
        let e = run_command(&argv, None, Duration::from_secs(10), &stop, false, |_| {
            Err::<(), _>(io::Error::other("broken pipe"))
        });
        assert_eq!(e.unwrap_err(), "true: reading output: broken pipe");
        let e = run_command(
            &argv,
            None,
            Duration::from_secs(10),
            &stop,
            false,
            |_| -> io::Result<()> { panic!("reader") },
        );
        assert_eq!(e.unwrap_err(), "true: reading output: its thread panicked");
    }

    #[test]
    fn output_is_read_whole() {
        let stop = AtomicBool::new(false);
        let argv = ["sh", "-c", "printf 'a\\nb'; echo err >&2"].map(String::from);
        let out = run_command(
            &argv,
            None,
            Duration::from_secs(10),
            &stop,
            false,
            |mut r| {
                let mut buf = Vec::new();
                r.read_to_end(&mut buf).map(|_| buf)
            },
        );
        assert_eq!(out.unwrap(), [b"a\nb".to_vec()]);
    }
}
