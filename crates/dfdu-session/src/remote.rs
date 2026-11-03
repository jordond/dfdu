//! Scans on another host through `ssh`: the agent there is started by a bootstrap script that
//! finds an installed or cached one, or receives one sent from here (`boot`, `fetch`), and then
//! speaks the wire protocol over ssh's standard streams. A front end says how ssh asks for
//! passwords (its environment, [`Ssh::env`]) and answers what the bootstrap asks ([`Front`]).

pub mod boot;
pub mod fetch;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use dfdu_engine::{Engine, EngineOptions, RemoteIo, RemoteRules};
use dfdu_scan::JobOptions;

use crate::startup::Source;

/// Which agent is started on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMode {
    /// The installed dfdu if it speaks this one's protocol, else one sent earlier, else send
    /// one (asking first).
    Auto,
    /// The dfdu installed on the host, as `ssh HOST dfdu agent`.
    Installed,
    /// One sent earlier, else send one, without asking.
    Upload,
}

/// What the front end is asked while the agent starts.
pub trait Front {
    /// Whether to send `what` (the agent, its build and size) into `dir` on the host, in
    /// [`AgentMode::Auto`]; `Err` says why not.
    ///
    /// # Errors
    ///
    /// The user declined, or could not be asked.
    fn allow(&mut self, what: &str, dir: &str) -> io::Result<()>;
    /// What is happening: downloading the agent, sending it.
    fn note(&mut self, msg: &str);
}

/// Where ssh's own messages go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stderr {
    /// Where the front end's go: a terminal, where ssh asks for passwords itself.
    Inherit,
    /// Kept, the last lines naming why ssh failed.
    Capture,
}

/// How to reach the host.
pub struct Ssh {
    /// What ssh is given as the destination: a name from its config, `user@host`, an address.
    pub host: String,
    pub port: Option<u16>,
    /// The private key to log in with, and only it.
    pub identity: Option<PathBuf>,
    pub mode: AgentMode,
    /// Set in ssh's environment (the askpass helper).
    pub env: Vec<(OsString, OsString)>,
    pub stderr: Stderr,
    pub agents: fetch::Agents,
    /// The ssh program and options of its own: `DFDU_REMOTE_SSH` (split at spaces) or `ssh`.
    pub program: Vec<OsString>,
    /// The whole command instead of ssh and the bootstrap: `DFDU_REMOTE_COMMAND` (split at
    /// spaces), for tests.
    pub replaced: Option<Vec<OsString>>,
}

impl Ssh {
    /// To `host` with ssh as the environment says, everything else left as ssh has it, its
    /// messages kept.
    #[must_use]
    pub fn new(host: String, mode: AgentMode, agents: fetch::Agents) -> Ssh {
        let split = |c: String| c.split_whitespace().map(OsString::from).collect::<Vec<_>>();
        Ssh {
            host,
            port: None,
            identity: None,
            mode,
            env: Vec::new(),
            stderr: Stderr::Capture,
            agents,
            program: std::env::var("DFDU_REMOTE_SSH")
                .map(split)
                .unwrap_or_else(|_| vec!["ssh".into()]),
            replaced: std::env::var("DFDU_REMOTE_COMMAND").ok().map(split),
        }
    }

    /// ssh's command line, and whether the bootstrap runs: the program, the port and key, then
    /// the host and the remote command; or [`Ssh::replaced`], without the bootstrap.
    fn argv(&self) -> (Vec<OsString>, bool) {
        if let Some(c) = &self.replaced {
            return (c.clone(), false);
        }
        let mut argv = self.program.clone();
        if let Some(p) = self.port {
            argv.extend(["-p".into(), p.to_string().into()]);
        }
        if let Some(key) = &self.identity {
            argv.extend(["-i".into(), key.clone().into_os_string()]);
            argv.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
        }
        argv.extend(["--".into(), self.host.clone().into()]);
        match self.mode {
            AgentMode::Installed => argv.extend(["dfdu".into(), "agent".into()]),
            AgentMode::Auto | AgentMode::Upload => argv.push(boot::command(self.mode).into()),
        }
        (argv, self.mode != AgentMode::Installed)
    }
}

/// Lines of ssh's messages kept with [`Stderr::Capture`].
const KEPT_LINES: usize = 8;

/// Stops a connection being made, from another thread: kills ssh, which ends what it waits for.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<Slot>>);

#[derive(Default)]
struct Slot {
    cancelled: bool,
    child: Option<Child>,
}

impl Cancel {
    pub fn cancel(&self) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.cancelled = true;
        if let Some(c) = &mut s.child {
            let _ = c.kill();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }

    /// Holds `child` to kill on cancel; `Err` (with it killed) when already cancelled.
    fn hold(&self, mut child: Child) -> io::Result<()> {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if s.cancelled {
            let _ = child.kill();
            let _ = child.wait();
            return Err(cancelled());
        }
        s.child = Some(child);
        Ok(())
    }

    fn take(&self) -> Option<Child> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .child
            .take()
    }
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

/// Starts ssh to the host and the agent there, as `ssh.mode` says, asking `front` what the
/// bootstrap needs. `cancel` stops it from another thread.
///
/// # Errors
///
/// ssh cannot be started or fails (the message is ssh's own last lines with
/// [`Stderr::Capture`]), the bootstrap fails, or it was cancelled
/// ([`io::ErrorKind::Interrupted`]).
pub fn connect(ssh: &Ssh, front: &mut dyn Front, cancel: &Cancel) -> io::Result<RemoteIo> {
    let (argv, boot) = ssh.argv();
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("the remote command is empty"))?;
    let name = prog.to_string_lossy().into_owned();
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .envs(ssh.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(match ssh.stderr {
            Stderr::Inherit => Stdio::inherit(),
            Stderr::Capture => Stdio::piped(),
        });
    no_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
    let pipe = |what| io::Error::other(format!("{name}: no {what} pipe"));
    let stdout = child.stdout.take().ok_or_else(|| pipe("stdout"));
    let stdin = child.stdin.take().ok_or_else(|| pipe("stdin"));
    let said = child.stderr.take().map(keep_lines);
    cancel.hold(child)?;
    let (mut reader, mut writer) = (BufReader::new(stdout?), stdin?);
    let failed = |e: io::Error, writer: std::process::ChildStdin| {
        drop(writer);
        let child = cancel.take();
        if cancel.is_cancelled() {
            if let Some(mut c) = child {
                let _ = c.wait();
            }
            return cancelled();
        }
        let code = child.and_then(|mut c| exited(&mut c));
        let lines = said.as_ref().map(Said::text).unwrap_or_default();
        match code {
            // ssh said why: on the terminal, or in the lines kept.
            Some(code) if code != 0 && e.kind() == io::ErrorKind::UnexpectedEof => {
                if lines.is_empty() {
                    io::Error::other(format!("{name} exited with status {code}"))
                } else {
                    io::Error::other(lines)
                }
            }
            _ => e,
        }
    };
    if boot {
        let mut supply = Sender {
            ssh,
            front,
            plan: Vec::new(),
        };
        if let Err(e) = boot::run(&mut reader, &mut writer, &mut supply) {
            return Err(failed(e, writer));
        }
    }
    if cancel.is_cancelled() {
        return Err(failed(cancelled(), writer));
    }
    let child = cancel.take();
    // The reader keeps what it read past the bootstrap: the start of the agent's hello.
    Ok(RemoteIo {
        reader: Box::new(reader),
        writer: Box::new(writer),
        child,
    })
}

/// No console window for ssh started from a window on Windows.
fn no_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// The last lines ssh wrote, read on a thread of their own until it exits.
#[derive(Clone)]
struct Said(Arc<Mutex<VecDeque<String>>>);

impl Said {
    fn text(&self) -> String {
        let lines = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn keep_lines(stderr: std::process::ChildStderr) -> Said {
    let said = Said(Arc::default());
    let kept = said.clone();
    let read = move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end().to_string();
            if line.is_empty() {
                continue;
            }
            let mut lines = kept.0.lock().unwrap_or_else(PoisonError::into_inner);
            if lines.len() == KEPT_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    };
    // Without the thread ssh's messages are lost, nothing more.
    let _ = std::thread::Builder::new()
        .name("dfdu-ssh-stderr".into())
        .spawn(read);
    said
}

/// The exit code of `child`, given a moment to exit after its output ended; killed if it
/// does not.
fn exited(child: &mut Child) -> Option<i32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The agents the bootstrap sends, asking the front end first in `auto` mode.
struct Sender<'a> {
    ssh: &'a Ssh,
    front: &'a mut dyn Front,
    /// What may be sent, preferred first.
    plan: Vec<fetch::Candidate>,
}

impl boot::Supply for Sender<'_> {
    fn keys(&mut self, target: &'static str) -> io::Result<Vec<String>> {
        self.plan = self.ssh.agents.plan(target)?;
        Ok(self.plan.iter().map(|c| c.key.clone()).collect())
    }

    fn allow(&mut self, target: &'static str, dir: &str) -> io::Result<()> {
        if self.ssh.mode == AgentMode::Upload {
            return Ok(());
        }
        self.front.allow(&what(self.plan.first(), target), dir)
    }

    fn send(&mut self, target: &'static str) -> io::Result<(String, Vec<u8>)> {
        let host = &self.ssh.host;
        let mut failed = None;
        for c in &self.plan {
            if let Some(e) = failed.take() {
                self.front
                    .note(&format!("{host}: {e}; sending the local dfdu instead"));
            }
            if c.size().is_none() {
                self.front.note(&format!(
                    "{host}: downloading the dfdu {VERSION} agent for {}",
                    c.built_for
                ));
            }
            match self.ssh.agents.load(target, c) {
                Ok((key, bin)) => {
                    self.front.note(&format!(
                        "{host}: sending dfdu agent ({}, {})",
                        c.built_for,
                        megabytes(bin.len() as u64)
                    ));
                    return Ok((key, bin));
                }
                Err(e) => failed = Some(e),
            }
        }
        Err(failed.unwrap_or_else(|| io::Error::other("no agent to send")))
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What would be sent, for the question: the local dfdu, or the release build, with its size.
fn what(first: Option<&fetch::Candidate>, target: &str) -> String {
    match first {
        Some(c) if c.is_this() => {
            let size = c.size().map(|n| format!(", {}", megabytes(n)));
            format!("this dfdu, {}{}", c.built_for, size.unwrap_or_default())
        }
        Some(c) => match c.size() {
            Some(n) => format!("{}, {}", c.built_for, megabytes(n)),
            None => format!("{}, downloaded from the release", c.built_for),
        },
        None => target.to_string(),
    }
}

/// `n` bytes in megabytes, to one decimal.
#[must_use]
pub fn megabytes(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
    let mb = n as f64 / 1e6;
    format!("{mb:.1} MB")
}

/// A scan on the host, through its agent: what [`open`] started. It can be sent to the
/// thread that opens it ([`Opened::source`]), unlike a [`Source`].
pub struct Opened {
    pub engine: Engine<dfdu_scan::NativeBackend>,
    /// The root, as the agent resolved it.
    pub path: dfdu_tree::HostPath,
    pub job: JobOptions,
    /// Problems with the rules, as the agent found them.
    pub errors: Vec<String>,
}

/// Starts an engine scanning `root` (as typed: the agent resolves it on its host) through the
/// agent of `io`, classifying with `rules`, with `job`'s other options.
///
/// # Errors
///
/// When the agent does not answer or speaks no common protocol version.
pub fn open(
    io: RemoteIo,
    root: &Path,
    rules: Option<RemoteRules>,
    opts: EngineOptions,
    job: JobOptions,
) -> io::Result<Opened> {
    let backend = dfdu_scan::native_backend(dfdu_sys::Storage::Unknown, false);
    let (engine, c) = Engine::start_remote(backend, opts, io, root, rules)?;
    Ok(Opened {
        engine,
        path: c.root,
        job: JobOptions {
            class: c.class,
            ..job
        },
        errors: c.errors,
    })
}

impl Opened {
    /// The source to open: the agent's tree, of another host.
    #[must_use]
    pub fn source(self) -> Source {
        Source::Remote {
            engine: self.engine,
            path: self.path,
            job: self.job,
            local: false,
        }
    }
}

#[cfg(test)]
mod tests;
