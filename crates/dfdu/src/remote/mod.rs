//! Scans on another host: `dfdu agent` scans there, and `--remote` starts it through `ssh`,
//! sending it first when the host has none (`boot`, `fetch`).

mod boot;
mod fetch;

use std::io::{BufReader, IsTerminal};
use std::path::Path;
use std::process::ExitCode;

use dfdu_engine::{Engine, EngineOptions};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{NativeBackend, native_backend};
use dfdu_tree::HostPath;

/// Which agent `--remote` starts on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentMode {
    /// The installed dfdu if it speaks this one's protocol, else one sent earlier, else send
    /// one (asking first).
    Auto,
    /// The dfdu installed on the host, as `ssh HOST dfdu agent`.
    Installed,
    /// One sent earlier, else send one, without asking.
    Upload,
}

/// `--remote-agent`'s help, the same for every command that has it.
pub const AGENT_MODE_HELP: &str = "How to start the agent on HOST: auto (an installed dfdu that \
    speaks this one's protocol, else one sent earlier, else send this version's, asking first), \
    installed, or upload (send one without asking) [default: auto, or DFDU_REMOTE_AGENT]";

/// The environment variable for `--remote-agent`'s default.
const AGENT_MODE_VAR: &str = "DFDU_REMOTE_AGENT";

impl AgentMode {
    /// `flag`, else `DFDU_REMOTE_AGENT`, else `auto`.
    pub fn resolve(flag: Option<AgentMode>) -> Result<AgentMode, ExitCode> {
        use clap::ValueEnum;
        if let Some(m) = flag {
            return Ok(m);
        }
        match std::env::var(AGENT_MODE_VAR) {
            Ok(v) if !v.is_empty() => AgentMode::from_str(&v, true).map_err(|_| {
                eprintln!("dfdu: {AGENT_MODE_VAR}={v}: expected auto, installed or upload");
                ExitCode::from(2)
            }),
            _ => Ok(AgentMode::Auto),
        }
    }
}

/// `dfdu agent`: the scanner for a dfdu on another host, over stdin and stdout. With
/// `protocols`, prints the protocol versions it speaks (`MIN MAX`) instead.
pub fn agent(threads: Option<usize>, protocols: bool) -> ExitCode {
    if protocols {
        println!(
            "{} {}",
            dfdu_engine::wire::MIN_VERSION,
            dfdu_engine::wire::VERSION
        );
        return ExitCode::SUCCESS;
    }
    let workers = threads.unwrap_or_else(|| dfdu_engine::workers_for(dfdu_sys::Storage::Unknown));
    let r = dfdu_engine::agent::run(
        native_backend(dfdu_sys::Storage::Unknown, false),
        workers,
        dfdu_clean::default_journal_dir(),
        std::io::stdin(),
        std::io::stdout(),
    );
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu agent: {e}");
            ExitCode::from(1)
        }
    }
}

/// Starts the agent on `host` through `ssh` (`DFDU_REMOTE_SSH`, split at spaces, replaces
/// `ssh`), as `mode` says. `DFDU_REMOTE_COMMAND`, split at spaces, replaces the whole command
/// and skips the bootstrap.
fn agent_io(host: &str, mode: AgentMode) -> std::io::Result<dfdu_engine::RemoteIo> {
    use std::process::{Command as Proc, Stdio};
    let split = |c: String| c.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let (argv, boot) = match std::env::var("DFDU_REMOTE_COMMAND") {
        Ok(c) => (split(c), false),
        Err(_) => {
            let mut argv = std::env::var("DFDU_REMOTE_SSH")
                .map(split)
                .unwrap_or_else(|_| vec!["ssh".into()]);
            argv.extend(["--".into(), host.into()]);
            match mode {
                AgentMode::Installed => argv.extend(["dfdu".into(), "agent".into()]),
                AgentMode::Auto | AgentMode::Upload => argv.push(boot::command(mode)),
            }
            (argv, mode != AgentMode::Installed)
        }
    };
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("the remote command is empty"))?;
    let mut child = Proc::new(prog)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| std::io::Error::new(e.kind(), format!("{prog}: {e}")))?;
    let pipe = |what| std::io::Error::other(format!("{prog}: no {what} pipe"));
    let mut reader = BufReader::new(child.stdout.take().ok_or_else(|| pipe("stdout"))?);
    let mut writer = child.stdin.take().ok_or_else(|| pipe("stdin"))?;
    if boot {
        let mut supply = Supply {
            host,
            mode,
            agents: fetch::Agents::from_env(),
            plan: Vec::new(),
        };
        if let Err(e) = boot::run(&mut reader, &mut writer, &mut supply) {
            drop(writer);
            return Err(match exited(&mut child) {
                // ssh said why on stderr.
                Some(code) if code != 0 && e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    std::io::Error::other(format!("{prog} exited with status {code}"))
                }
                _ => e,
            });
        }
    }
    // The reader keeps what it read past the bootstrap: the start of the agent's hello.
    Ok(dfdu_engine::RemoteIo {
        reader: Box::new(reader),
        writer: Box::new(writer),
        child: Some(child),
    })
}

/// The exit code of `child`, given a moment to exit after its output ended; killed if it
/// does not.
fn exited(child: &mut std::process::Child) -> Option<i32> {
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

/// The agents `boot` sends, asking the user first in `auto` mode.
struct Supply<'a> {
    host: &'a str,
    mode: AgentMode,
    agents: fetch::Agents,
    /// What may be sent, preferred first.
    plan: Vec<fetch::Candidate>,
}

impl boot::Supply for Supply<'_> {
    fn keys(&mut self, target: &'static str) -> std::io::Result<Vec<String>> {
        self.plan = self.agents.plan(target)?;
        Ok(self.plan.iter().map(|c| c.key.clone()).collect())
    }

    fn allow(&mut self, target: &'static str, dir: &str) -> std::io::Result<()> {
        let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let what = match self.plan.first() {
            Some(c) if c.is_this() => {
                let size = c.size().map(|n| format!(", {}", megabytes(n)));
                format!("this dfdu, {}{}", c.built_for, size.unwrap_or_default())
            }
            Some(c) => match c.size() {
                Some(n) => format!("{}, {}", c.built_for, megabytes(n)),
                None => format!("{}, downloaded from the release", c.built_for),
            },
            None => target.to_string(),
        };
        boot::decide(self.mode, terminal, || {
            eprint!(
                "{} has no dfdu {} agent. Send one ({what}) to {dir}? [y/N] ",
                self.host,
                env!("CARGO_PKG_VERSION")
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(line)
        })
        .map_err(|hint| {
            std::io::Error::other(format!(
                "no dfdu {} agent there; {hint}",
                env!("CARGO_PKG_VERSION")
            ))
        })
    }

    fn send(&mut self, target: &'static str) -> std::io::Result<(String, Vec<u8>)> {
        let mut failed = None;
        for c in &self.plan {
            if let Some(e) = failed.take() {
                eprintln!("dfdu: {}: {e}; sending this dfdu instead", self.host);
            }
            if c.size().is_none() {
                eprintln!(
                    "dfdu: {}: downloading the dfdu {} agent for {}",
                    self.host,
                    env!("CARGO_PKG_VERSION"),
                    c.built_for
                );
            }
            match self.agents.load(target, c) {
                Ok((key, bin)) => {
                    eprintln!(
                        "dfdu: {}: sending dfdu agent ({}, {})",
                        self.host,
                        c.built_for,
                        megabytes(bin.len() as u64)
                    );
                    return Ok((key, bin));
                }
                Err(e) => failed = Some(e),
            }
        }
        Err(failed.unwrap_or_else(|| std::io::Error::other("no agent to send")))
    }
}

fn megabytes(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
    let mb = n as f64 / 1e6;
    format!("{mb:.1} MB")
}

/// An engine scanning on `host` through its agent, classifying with `rules`, what the scan
/// root classifies against, and the root to scan: `root` as typed, resolved by the agent on its
/// host. `mode` is `--remote-agent`. Reports a failure to start.
pub fn engine(
    host: &str,
    opts: EngineOptions,
    root: &Path,
    rules: Option<dfdu_engine::RemoteRules>,
    mode: Option<AgentMode>,
) -> Result<(Engine<NativeBackend>, Option<ClassOpts>, HostPath), ExitCode> {
    let mode = AgentMode::resolve(mode)?;
    let started = agent_io(host, mode).and_then(|io| {
        Engine::start_remote(
            native_backend(dfdu_sys::Storage::Unknown, false),
            opts,
            io,
            root,
            rules,
        )
    });
    match started {
        Ok((e, c)) => {
            for msg in c.errors {
                eprintln!("dfdu: {host}: {msg}");
            }
            Ok((e, c.class, c.root))
        }
        Err(e) => {
            eprintln!("dfdu: {host}: {e}");
            Err(ExitCode::from(1))
        }
    }
}
