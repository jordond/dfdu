//! The remote scan's wire format: what `dfdu agent` on another host
//! and the local side say to each other over the agent's stdin and stdout.
//!
//! A frame is a little-endian `u32` payload length, the payload's CRC32C as a little-endian
//! `u32`, then the payload; payloads over [`MAX_FRAME`] are refused. A payload is one message,
//! [`FromAgent`] or [`ToAgent`], its first byte the message's tag. Integers are LEB128 varints
//! (signed ones zigzag-encoded), byte strings a varint length and the bytes.
//!
//! The agent's messages are checked as they are read: every name span lies inside its chunk's
//! names, every mark names an entry of its chunk, every class a rule the local side has, so a
//! bad agent or a corrupted link gives an error, never a panic or a wrong tree.
//!
//! Classification runs on the agent, with the local side's rules: [`ToAgent::Rules`] carries
//! their text, the agent builds them for its own platform and environment and answers with
//! [`FromAgent::Rules`], from which the local side builds the same set. Both number the rules
//! alike (checked by [`RuleSet::fingerprint`]), so classes travel as rule numbers.

mod codec;
mod delete;
mod jobs;
mod message;
mod scan;
#[cfg(test)]
mod tests;

use std::io::{self, Read, Write};

use crate::classifier::{CheckMsg, CheckReq};
use crate::measure::{MeasureMsg, MeasureReq};
use dfdu_clean::{DeleterMsg, DeleterReq};
use dfdu_rules::protected::State;
use dfdu_rules::{Hit, RuleSet};
use dfdu_tree::RuleIx;

use dfdu_scan::{DirId, JobId, ScanMsg};

pub use codec::{Dec, Enc};

/// The protocol version this build speaks. Both sides speak exactly one, so any change to a
/// message's encoding takes a new number.
pub const VERSION: u32 = 6;

/// The oldest protocol version this build speaks: [`VERSION`], since it speaks only one.
pub const MIN_VERSION: u32 = VERSION;

/// The largest payload accepted.
pub const MAX_FRAME: usize = 64 << 20;

#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    /// A frame longer than [`MAX_FRAME`].
    TooLarge(u64),
    /// The payload does not match its checksum.
    Checksum,
    /// The payload is not a valid message.
    Malformed(&'static str),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "{e}"),
            WireError::TooLarge(n) => write!(f, "frame of {n} bytes is too large"),
            WireError::Checksum => write!(f, "frame checksum mismatch"),
            WireError::Malformed(what) => write!(f, "malformed message: {what}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

/// Writes one frame holding `payload`.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] if `payload` is over [`MAX_FRAME`], or the error writing
/// to `w`.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = match u32::try_from(payload.len()) {
        Ok(len) if payload.len() <= MAX_FRAME => len,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame too large",
            ));
        }
    };
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(&len.to_le_bytes());
    head[4..].copy_from_slice(&crc32c::crc32c(payload).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(payload)
}

fn cut() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the link ended inside a frame",
    )
}

/// Reads one frame into `buf`. `Ok(false)` at the end of the stream before a frame starts.
///
/// # Errors
///
/// [`WireError::TooLarge`] for a frame over [`MAX_FRAME`], [`WireError::Checksum`] for one
/// that does not match its checksum, and [`WireError::Io`] if reading fails or the stream
/// ends inside a frame.
pub fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> Result<bool, WireError> {
    let mut head = [0u8; 8];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(cut().into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let [l0, l1, l2, l3, c0, c1, c2, c3] = head;
    let len = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
    let len = match usize::try_from(len) {
        Ok(len) if len <= MAX_FRAME => len,
        _ => return Err(WireError::TooLarge(len)),
    };
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => cut(),
        _ => e,
    })?;
    if crc32c::crc32c(buf) != u32::from_le_bytes([c0, c1, c2, c3]) {
        return Err(WireError::Checksum);
    }
    Ok(true)
}

/// A message from the agent.
#[derive(Clone, Debug)]
pub enum FromAgent {
    /// First message: the versions the agent speaks and its program version.
    Hello { min: u32, max: u32, tool: String },
    /// Scanner messages, in the order the agent's scanner sent them.
    Batch(Vec<ScanMsg>),
    /// Nothing to send for a while; the link is alive.
    Heartbeat,
    /// The answer to [`ToAgent::Rules`]: what the agent built the rules for (its platform, the
    /// variables they read and the path patterns that lead elsewhere through a link on its
    /// disks, [`dfdu_rules::Env::links_followed`], so the local side can build the same set),
    /// their fingerprint, and what the scan root classifies against.
    Rules {
        platform: String,
        vars: Vec<(String, String)>,
        links: Vec<(String, String)>,
        fingerprint: u64,
        root: WireClass,
    },
    /// A job started ([`ToAgent::StartJob`]), with its submitted sequence number.
    Started { job: JobId, seq: u64 },
    /// The answer to [`ToAgent::Abandon`]: whether no worker was still inside the directory.
    Abandoned {
        job: JobId,
        dir: DirId,
        complete: bool,
    },
    /// The agent cannot go on.
    Fatal(String),
    /// From the agent's deleter ([`ToAgent::Delete`]).
    Deleter(DeleterMsg),
    /// The answers to a [`ToAgent::Check`].
    Checked(Vec<CheckMsg>),
    /// Sent once the version is agreed: the smallest allocation worth measuring on the agent's
    /// host (1 MiB on macOS, where each file costs a lookup), 0 when it cannot measure; and
    /// whether it runs as root (read-only btrfs snapshots can be deleted); the kind of host,
    /// which the local side builds the host's paths for; and the scan root of
    /// [`ToAgent::Hello`] as the host resolves it (canonical, or at least absolute).
    Host {
        measure_min: u64,
        privileged: bool,
        host: dfdu_tree::Host,
        root: Vec<u8>,
    },
    /// The answers to one [`ToAgent::Measure`] request.
    Measured(Vec<MeasureMsg>),
}

/// A message to the agent.
#[derive(Clone, Debug)]
pub enum ToAgent {
    /// First message: the version chosen, and the scan root as given, which only the agent's
    /// host can resolve.
    Hello {
        version: u32,
        root: Vec<u8>,
    },
    /// The rules to classify with: rule files in load order, `None` naming the built-in pack
    /// (the text is still sent, so both sides have the same one). The agent answers with
    /// [`FromAgent::Rules`], resolving `root` (a path on its host) as a scan root.
    Rules {
        files: Vec<(Option<String>, String)>,
        root: Vec<u8>,
    },
    /// Scan `root` (a path on the agent's host) as job `job`, classifying against `class`
    /// with the rules of [`ToAgent::Rules`]; `enter` as [`dfdu_scan::JobOptions::enter`].
    ///
    /// [`dfdu_scan::JobOptions::keep`] (the directories a resume keeps as loaded) is not
    /// sent: the agent reads every directory, so a resume's rescan through it lists its
    /// subtree in full, slower but to the same tree, and the agent never reports
    /// [`SkipReason::Kept`](dfdu_scan::SkipReason::Kept). Resumes are meant for local scans
    /// anyway: the change journal they ask is this host's.
    StartJob {
        job: JobId,
        root: Vec<u8>,
        cross_mounts: bool,
        network: bool,
        enter: Vec<(u64, u64)>,
        class: Option<WireClass>,
    },
    EndJob {
        job: JobId,
    },
    StopDirs {
        job: JobId,
        dirs: Vec<DirId>,
    },
    Abandon {
        job: JobId,
        dir: DirId,
    },
    /// For the agent's deleter, which journals and verifies on the agent's host. A native
    /// command is sent as its rule and path, and the agent builds it from its own rules.
    Delete(DeleterReq),
    /// Stop the running plan after the current item.
    AbortDelete,
    /// Guard checks of classes (in use, tracked by git) and native size commands (sent as
    /// their rule) to run on the agent's host.
    Check(Vec<CheckReq>),
    /// Files to measure on the agent's host, one directory per request.
    Measure(Vec<MeasureReq>),
}

/// What a job classifies against: [`dfdu_scan::classify::ClassOpts`] without the rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireClass {
    pub path: Vec<u8>,
    pub inside: Vec<RuleIx>,
    pub protection: State,
    pub chain: Vec<Hit>,
}

impl WireClass {
    #[must_use]
    pub fn of(c: &dfdu_scan::classify::ClassOpts) -> WireClass {
        WireClass {
            path: c.path.clone(),
            inside: c.inside.clone(),
            protection: c.protection,
            chain: c.chain.clone(),
        }
    }

    #[must_use]
    pub fn with(self, rules: std::sync::Arc<RuleSet>) -> dfdu_scan::classify::ClassOpts {
        dfdu_scan::classify::ClassOpts {
            rules,
            path: self.path,
            inside: self.inside,
            protection: self.protection,
            chain: self.chain,
        }
    }

    /// Whether every rule named is one of `rules`.
    #[must_use]
    pub fn fits(&self, rules: &RuleSet) -> bool {
        self.inside.iter().all(|&r| known(rules, r, None))
            && self.chain.iter().all(|h| known(rules, h.rule, h.degraded))
    }
}

/// Whether `rule` is one of `rules` and `degraded` one of its conditions.
fn known(rules: &RuleSet, rule: RuleIx, degraded: Option<u16>) -> bool {
    rules
        .rules()
        .get(usize::from(rule.0))
        .is_some_and(|r| degraded.is_none_or(|d| (d as usize) < r.degrade.len()))
}
