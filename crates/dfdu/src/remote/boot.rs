//! Starting the agent on a host that may have no dfdu: a POSIX sh script (`boot.sh`), run
//! through ssh, finds an installed or cached agent or receives one on its stdin, then becomes
//! the agent, so the same pipes carry the wire protocol afterwards.
//!
//! The script talks in lines on its stdout, the client answers on its stdin:
//!
//! - `DFDU-BOOT 1 <os> <arch>`, after whatever the remote shell's rc files printed
//! - `INSTALLED` (the dfdu on `PATH` speaks our protocol; the agent follows) or `KEY?`, to
//!   which the client answers `KEY <key>...`, the keys it would accept, preferred first
//! - `HAVE` (an agent cached under one of them runs; it follows), `SEND <dir>`, to which the
//!   client answers `<key> <size> <sha256>` and the bytes (or closes its stdin to decline), or
//!   `ERR <why>`
//! - after the bytes: `OK` (the agent follows), `BAD` (they arrived corrupted) or `ERR <why>`

use std::io::{self, BufRead, Read, Write};

use super::AgentMode;

/// The script. It holds no `'` or `\`, so it survives single quotes in any of sh, bash, zsh
/// and fish.
pub const SCRIPT: &str = include_str!("boot.sh");

/// The remote command: `sh -c` running [`SCRIPT`] with our protocol version and `mode`.
pub fn command(mode: AgentMode) -> String {
    let mode = match mode {
        AgentMode::Upload => "upload",
        AgentMode::Auto | AgentMode::Installed => "auto",
    };
    format!(
        "sh -c '{SCRIPT}' dfdu-boot {} {mode}",
        dfdu_engine::wire::VERSION
    )
}

/// How the agent was started.
#[derive(Debug, PartialEq, Eq)]
pub enum How {
    /// The dfdu installed on the host.
    Installed,
    /// One sent earlier and kept there.
    Cached,
    /// One sent just now.
    Uploaded,
}

/// Where the agent to send comes from, and whether to send it.
pub trait Supply {
    /// The keys of the agents for `target` the host may have kept, preferred first.
    fn keys(&mut self, target: &'static str) -> io::Result<Vec<String>>;
    /// Whether to send one, into `dir` on the host; `Err` says why not.
    fn allow(&mut self, target: &'static str, dir: &str) -> io::Result<()>;
    /// The agent to send: its key and bytes.
    fn send(&mut self, target: &'static str) -> io::Result<(String, Vec<u8>)>;
}

/// Lines the rc files on the host may print before the marker.
const NOISE_LINES: usize = 64;
/// Bytes they may print.
const NOISE_BYTES: u64 = 16 << 10;
/// The longest line the script prints after the marker.
const LINE: u64 = 4096;
const MARKER: &[u8] = b"DFDU-BOOT 1 ";

/// Runs the bootstrap on the script's stdout `r` and stdin `w`: on success the agent is
/// running and the next bytes on `r` are its own. Ends with [`io::ErrorKind::UnexpectedEof`]
/// when the connection closes before the script starts (ssh failed).
pub fn run(r: &mut impl BufRead, w: &mut impl Write, supply: &mut impl Supply) -> io::Result<How> {
    let (os, arch) = marker(r)?;
    match line(r)?.as_str() {
        "INSTALLED" => return Ok(How::Installed),
        "KEY?" => {}
        other => return Err(status_error(other)),
    }
    let target = super::fetch::target_for(&os, &arch).ok_or_else(|| {
        io::Error::other(format!("no dfdu build for {os} {arch}; install dfdu there"))
    })?;
    let keys = supply.keys(target)?;
    writeln!(w, "KEY {}", keys.join(" "))?;
    w.flush()?;
    let reply = line(r)?;
    let dir = match reply.split_once(' ') {
        _ if reply == "HAVE" => return Ok(How::Cached),
        Some(("SEND", dir)) => dir.to_string(),
        _ => return Err(status_error(&reply)),
    };
    supply.allow(target, &dir)?;
    let (key, bytes) = supply.send(target)?;
    let sent = (|| {
        let hash = super::fetch::sha256_hex(&bytes);
        writeln!(w, "{key} {} {hash}", bytes.len())?;
        w.write_all(&bytes)?;
        w.flush()
    })();
    // A script that stopped reading says why before it exits.
    match (sent, line(r)) {
        (_, Ok(s)) if s == "OK" => Ok(How::Uploaded),
        (_, Ok(s)) => Err(status_error(&s)),
        (Err(e), Err(_)) | (Ok(()), Err(e)) => Err(e),
    }
}

/// Skips what the shell printed before the marker; the host's os and architecture.
fn marker(r: &mut impl BufRead) -> io::Result<(String, String)> {
    let mut seen = 0u64;
    for _ in 0..NOISE_LINES {
        let mut buf = Vec::new();
        let n = r
            .by_ref()
            .take(NOISE_BYTES - seen + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the connection closed before the bootstrap started",
            ));
        }
        seen += n as u64;
        // Output without a newline from the rc files may come first on the line.
        if let Some(at) = buf.windows(MARKER.len()).position(|w| w == MARKER) {
            let rest = clean(&buf[at + MARKER.len()..]);
            let mut words = rest.split_whitespace();
            if let (Some(os), Some(arch)) = (words.next(), words.next()) {
                return Ok((os.to_string(), arch.to_string()));
            }
        }
        if seen > NOISE_BYTES {
            break;
        }
    }
    Err(io::Error::other(
        "no bootstrap marker from the host: is its shell sh, bash, zsh or fish? (or install dfdu \
         there and pass --remote-agent installed)",
    ))
}

/// The script's next status line.
fn line(r: &mut impl BufRead) -> io::Result<String> {
    let mut buf = Vec::new();
    r.by_ref().take(LINE).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the host closed the connection while starting the agent",
        ));
    }
    Ok(clean(&buf).trim_end().to_string())
}

/// Text from the host, with control characters replaced so it cannot drive the terminal.
fn clean(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| if c.is_control() && c != '\n' { '?' } else { c })
        .collect()
}

fn status_error(status: &str) -> io::Error {
    match status.split_once(' ') {
        _ if status == "BAD" => io::Error::other("the agent arrived corrupted"),
        Some(("ERR", why)) => io::Error::other(why.to_string()),
        _ => io::Error::other(format!(
            "unexpected reply while starting the agent: {status:?}"
        )),
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

/// Whether to send an agent in `mode`: `upload` always does, `auto` asks when `terminal`
/// (`answer` reads the reply) and refuses otherwise, so scripts never upload unasked.
pub fn decide(
    mode: AgentMode,
    terminal: bool,
    answer: impl FnOnce() -> io::Result<String>,
) -> Result<(), Declined> {
    match mode {
        AgentMode::Upload => Ok(()),
        _ if !terminal => Err(Declined),
        _ => match answer() {
            Ok(a) if matches!(a.trim(), "y" | "Y" | "yes" | "Yes") => Ok(()),
            _ => Err(Declined),
        },
    }
}

#[cfg(test)]
mod tests;
