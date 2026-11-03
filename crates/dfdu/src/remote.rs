//! Scans on another host: `dfdu agent` scans there, and `--remote` starts it through `ssh`,
//! sending it first when the host has none (`dfdu_session::remote`).

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use dfdu_engine::{Engine, EngineOptions};
use dfdu_scan::{JobOptions, NativeBackend, native_backend};
use dfdu_session::remote;
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

impl From<AgentMode> for remote::AgentMode {
    fn from(m: AgentMode) -> Self {
        match m {
            AgentMode::Auto => remote::AgentMode::Auto,
            AgentMode::Installed => remote::AgentMode::Installed,
            AgentMode::Upload => remote::AgentMode::Upload,
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
        dfdu_sys::dirs::journal_dir(),
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

/// Starts the agent on `host` through `ssh`, as `mode` says, asking on the terminal before
/// sending one; ssh asks for passwords there itself.
fn agent_io(host: &str, mode: AgentMode) -> std::io::Result<dfdu_engine::RemoteIo> {
    let this = std::env::current_exe().ok().zip(remote::fetch::own());
    let ssh = remote::Ssh {
        stderr: remote::Stderr::Inherit,
        ..remote::Ssh::new(
            host.to_string(),
            mode.into(),
            remote::fetch::Agents::from_env(this),
        )
    };
    remote::connect(&ssh, &mut Terminal { host }, &remote::Cancel::default())
}

/// Asks on the terminal, and says what happens on standard error.
struct Terminal<'a> {
    host: &'a str,
}

impl remote::Front for Terminal<'_> {
    fn allow(&mut self, what: &str, dir: &str) -> std::io::Result<()> {
        let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        decide(terminal, || {
            eprint!(
                "{} has no dfdu {} agent. Send one ({what}) to {dir}? [y/N] ",
                self.host,
                env!("CARGO_PKG_VERSION")
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(line)
        })
        .map_err(std::io::Error::other)
    }

    fn note(&mut self, msg: &str) {
        eprintln!("dfdu: {msg}");
    }
}

/// An agent not sent: the user said no, or could not be asked.
#[derive(Debug)]
pub struct Declined;

impl std::fmt::Display for Declined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no dfdu {} agent there; install dfdu there or pass --remote-agent upload",
            env!("CARGO_PKG_VERSION")
        )
    }
}

impl std::error::Error for Declined {}

/// Whether to send an agent in `auto` mode (`upload` never asks): asks when `terminal`
/// (`answer` reads the reply) and refuses otherwise, so scripts never upload unasked.
fn decide(
    terminal: bool,
    answer: impl FnOnce() -> std::io::Result<String>,
) -> Result<(), Declined> {
    if !terminal {
        return Err(Declined);
    }
    match answer() {
        Ok(a) if matches!(a.trim(), "y" | "Y" | "yes" | "Yes") => Ok(()),
        _ => Err(Declined),
    }
}

/// An engine scanning on `host` through its agent, classifying with `rules`, the options of
/// the scan (`job` with what the scan root classifies against), and the root to scan: `root` as
/// typed, resolved by the agent on its host. `mode` is `--remote-agent`. Reports a failure to
/// start.
pub fn engine(
    host: &str,
    opts: EngineOptions,
    job: JobOptions,
    root: &Path,
    rules: Option<dfdu_engine::RemoteRules>,
    mode: Option<AgentMode>,
) -> Result<(Engine<NativeBackend>, JobOptions, HostPath), ExitCode> {
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
            let job = JobOptions {
                class: c.class,
                ..job
            };
            Ok((e, job, c.root))
        }
        Err(e) => {
            eprintln!("dfdu: {host}: {e}");
            Err(ExitCode::from(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_decision() {
        let never = || -> std::io::Result<String> { panic!("must not ask") };
        let refused = decide(false, never).unwrap_err().to_string();
        assert!(refused.contains("--remote-agent upload"), "{refused}");
        assert!(refused.contains(env!("CARGO_PKG_VERSION")), "{refused}");
        assert!(decide(true, || Ok("y\n".into())).is_ok());
        assert!(decide(true, || Ok("\n".into())).is_err());
        assert!(decide(true, || Ok("n".into())).is_err());
        assert!(decide(true, || Err(std::io::Error::other("no tty"))).is_err());
    }
}
