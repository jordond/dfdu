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

use std::io::{self, Read, Write};

use std::path::PathBuf;
use std::sync::Arc;

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Measurement;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use dfdu_clean::inside::{Listing, Listings};
use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal};
use dfdu_rules::protected::State;
use dfdu_rules::{Hit, Inside, RuleIx, RuleSet, Safety};
use dfdu_tree::{Class, ErrKind, Kind, Meta, NodeRef, Subvol};

use dfdu_scan::{
    Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, ListingStatus, Mark, NameSpan,
    Probe, ScanMsg, SkipReason, SubdirRec,
};

/// The protocol version this build speaks.
pub const VERSION: u32 = 4;

/// The largest payload accepted.
pub const MAX_FRAME: usize = 64 << 20;

/// btrfs data profile names, by their number on the wire.
const PROFILES: [&str; 9] = [
    "single", "DUP", "RAID0", "RAID1", "RAID1C3", "RAID1C4", "RAID10", "RAID5", "RAID6",
];

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
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
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
    let len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(WireError::TooLarge(len as u64));
    }
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => cut(),
        _ => e,
    })?;
    if crc32c::crc32c(buf) != u32::from_le_bytes(head[4..].try_into().unwrap()) {
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
    /// The answer to [`ToAgent::Rules`]: what the agent built the rules for (its platform and
    /// the variables they read, so the local side can build the same set), their
    /// fingerprint, and what the scan root classifies against.
    Rules {
        platform: String,
        vars: Vec<(String, String)>,
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
    /// whether it runs as root (read-only btrfs snapshots can be deleted).
    Host { measure_min: u64, privileged: bool },
    /// The answers to one [`ToAgent::Measure`] request.
    Measured(Vec<MeasureMsg>),
}

/// A message to the agent.
#[derive(Clone, Debug)]
pub enum ToAgent {
    /// First message: the version chosen.
    Hello {
        version: u32,
    },
    /// The rules to classify with: rule files in load order, `None` naming the built-in pack
    /// (the text is still sent, so both sides have the same one). The agent answers with
    /// [`FromAgent::Rules`], resolving `root` (a path on its host) as a scan root.
    Rules {
        files: Vec<(Option<String>, String)>,
        root: Vec<u8>,
    },
    /// Scan `root` (a path on the agent's host) as job `job`, classifying against `class`
    /// with the rules of [`ToAgent::Rules`].
    StartJob {
        job: JobId,
        root: Vec<u8>,
        cross_mounts: bool,
        network: bool,
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
    pub fn of(c: &dfdu_scan::classify::ClassOpts) -> WireClass {
        WireClass {
            path: c.path.clone(),
            inside: c.inside.clone(),
            protection: c.protection,
            chain: c.chain.clone(),
        }
    }

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

/// Appends to a payload.
#[derive(Default)]
pub struct Enc(pub Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn var(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn zig(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)) as u64);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.var(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    fn bool(&mut self, b: bool) {
        self.u8(b as u8);
    }
    fn rule(&mut self, r: RuleIx) {
        self.var(r.0.into());
    }
    fn safety(&mut self, s: Safety) {
        self.u8(s as u8);
    }
}

/// Reads a payload.
pub struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, WireError>;

fn bad<T>(what: &'static str) -> R<T> {
    Err(WireError::Malformed(what))
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Dec { buf, pos: 0 }
    }
    fn u8(&mut self) -> R<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(WireError::Malformed("truncated"))?;
        self.pos += 1;
        Ok(b)
    }
    fn var(&mut self) -> R<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return bad("varint overflow");
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bad("varint overflow")
    }
    fn u32(&mut self) -> R<u32> {
        u32::try_from(self.var()?).or(bad("u32 out of range"))
    }
    fn u16(&mut self) -> R<u16> {
        u16::try_from(self.var()?).or(bad("u16 out of range"))
    }
    fn rule(&mut self) -> R<RuleIx> {
        self.u16().map(RuleIx)
    }
    fn safety(&mut self) -> R<Safety> {
        Safety::from_u8(self.u8()?).ok_or(WireError::Malformed("safety"))
    }
    fn zig(&mut self) -> R<i64> {
        let v = self.var()?;
        Ok((v >> 1) as i64 ^ -((v & 1) as i64))
    }
    fn bytes(&mut self) -> R<&'a [u8]> {
        let n = self.var()?;
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| self.pos.checked_add(n))
            .filter(|&e| e <= self.buf.len())
            .ok_or(WireError::Malformed("truncated"))?;
        let b = &self.buf[self.pos..end];
        self.pos = end;
        Ok(b)
    }
    fn lossy(&mut self) -> R<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    fn u64_le(&mut self) -> R<u64> {
        let end = self.pos.checked_add(8).filter(|&e| e <= self.buf.len());
        let end = end.ok_or(WireError::Malformed("truncated"))?;
        let v = u64::from_le_bytes(self.buf[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }
    fn text(&mut self) -> R<String> {
        match std::str::from_utf8(self.bytes()?) {
            Ok(t) => Ok(t.to_owned()),
            Err(_) => bad("text not UTF-8"),
        }
    }
    fn bool(&mut self) -> R<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => bad("bool"),
        }
    }
    /// A count of items each taking at least `min` bytes, checked against what is left, so a
    /// forged count cannot make a huge allocation.
    fn count(&mut self, min: usize) -> R<usize> {
        let n = self.var()?;
        match usize::try_from(n) {
            Ok(n) if n.saturating_mul(min.max(1)) <= self.buf.len() - self.pos => Ok(n),
            _ => bad("count larger than the payload"),
        }
    }
    fn end(&self) -> R<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            bad("trailing bytes")
        }
    }
}

fn enc_meta(e: &mut Enc, m: &Meta) {
    e.u8(match m.kind {
        Kind::File => 0,
        Kind::Dir => 1,
        Kind::Symlink => 2,
        Kind::Other => 3,
    });
    e.var(m.apparent);
    e.var(m.alloc);
    e.zig(m.mtime);
    e.var(m.dev);
    e.var(m.ino);
    e.var(m.nlink.into());
    e.var(m.mount);
}

fn dec_meta(d: &mut Dec) -> R<Meta> {
    Ok(Meta {
        kind: match d.u8()? {
            0 => Kind::File,
            1 => Kind::Dir,
            2 => Kind::Symlink,
            3 => Kind::Other,
            _ => return bad("kind"),
        },
        apparent: d.var()?,
        alloc: d.var()?,
        mtime: d.zig()?,
        dev: d.var()?,
        ino: d.var()?,
        nlink: d.u32()?,
        mount: d.var()?,
    })
}

fn enc_err(e: &mut Enc, k: &ErrKind) {
    match *k {
        ErrKind::PermissionDenied => e.u8(0),
        ErrKind::NotFound => e.u8(1),
        ErrKind::Os(n) => {
            e.u8(2);
            e.zig(n.into());
        }
        ErrKind::Stalled => e.u8(3),
        ErrKind::Other => e.u8(4),
    }
}

fn dec_err(d: &mut Dec) -> R<ErrKind> {
    Ok(match d.u8()? {
        0 => ErrKind::PermissionDenied,
        1 => ErrKind::NotFound,
        2 => ErrKind::Os(i32::try_from(d.zig()?).or(bad("errno"))?),
        3 => ErrKind::Stalled,
        4 => ErrKind::Other,
        _ => return bad("error kind"),
    })
}

fn enc_class(e: &mut Enc, c: &Class) {
    e.rule(c.rule);
    e.safety(c.safety);
    match c.degraded {
        Some(r) => {
            e.u8(1);
            e.var(r.into());
        }
        None => e.u8(0),
    }
    e.u8(c.flags);
}

/// A class naming one of `rules` rules; none are known without rules.
fn dec_class(d: &mut Dec, rules: Option<&RuleSet>) -> R<Class> {
    let rule = d.rule()?;
    let safety = d.safety()?;
    let degraded = match d.u8()? {
        0 => None,
        1 => Some(d.u16()?),
        _ => return bad("degraded"),
    };
    if !rules.is_some_and(|r| known(r, rule, degraded)) {
        return bad("class of an unknown rule");
    }
    Ok(Class {
        rule,
        safety,
        degraded,
        flags: d.u8()?,
    })
}

fn enc_wclass(e: &mut Enc, c: &WireClass) {
    e.bytes(&c.path);
    e.var(c.inside.len() as u64);
    for &r in &c.inside {
        e.rule(r);
    }
    e.u8(c.protection as u8);
    e.var(c.chain.len() as u64);
    for h in &c.chain {
        e.rule(h.rule);
        e.safety(h.safety);
        match h.degraded {
            Some(r) => {
                e.u8(1);
                e.var(r.into());
            }
            None => e.u8(0),
        }
        e.bool(h.inside == Inside::Check);
        e.bool(h.open);
    }
}

/// A class to classify against; with `rules`, every rule it names must be one of them.
fn dec_wclass(d: &mut Dec, rules: Option<&RuleSet>) -> R<WireClass> {
    let path = d.bytes()?.to_vec();
    let n = d.count(1)?;
    let mut inside = Vec::with_capacity(n);
    for _ in 0..n {
        inside.push(d.rule()?);
    }
    let protection = match d.u8()? {
        0 => State::Off,
        1 => State::On,
        2 => State::Sticky,
        _ => return bad("protection"),
    };
    let n = d.count(5)?;
    let mut chain = Vec::with_capacity(n);
    for _ in 0..n {
        let rule = d.rule()?;
        let safety = d.safety()?;
        let degraded = match d.u8()? {
            0 => None,
            1 => Some(d.u16()?),
            _ => return bad("degraded"),
        };
        let inside = if d.bool()? {
            Inside::Check
        } else {
            Inside::Done
        };
        chain.push(Hit {
            rule,
            safety,
            degraded,
            inside,
            open: d.bool()?,
        });
    }
    let c = WireClass {
        path,
        inside,
        protection,
        chain,
    };
    if rules.is_some_and(|r| !c.fits(r)) {
        return bad("class of an unknown rule");
    }
    Ok(c)
}

fn enc_span(e: &mut Enc, s: NameSpan) {
    e.var(s.off.into());
    e.var(s.len.into());
}

fn dec_span(d: &mut Dec, names: usize) -> R<NameSpan> {
    let s = NameSpan {
        off: d.u32()?,
        len: d.u16()?,
    };
    if s.off as usize + s.len as usize > names {
        return bad("name outside the chunk's names");
    }
    Ok(s)
}

fn enc_probe(e: &mut Enc, p: &Probe) {
    match &p.subvol {
        Some(s) => {
            e.u8(1);
            e.var(s.id);
            e.0.extend_from_slice(&s.uuid);
            match s.parent_uuid {
                Some(u) => {
                    e.u8(1);
                    e.0.extend_from_slice(&u);
                }
                None => e.u8(0),
            }
            e.bool(s.read_only);
            e.zig(s.otime);
            match s.qgroup {
                Some((r, x)) => {
                    e.u8(1);
                    e.var(r);
                    e.var(x);
                }
                None => e.u8(0),
            }
        }
        None => e.u8(0),
    }
    e.bool(p.shares_extents);
    match p.profile {
        Some((name, ratio)) => {
            e.u8(1);
            e.u8(PROFILES.iter().position(|p| *p == name).unwrap_or(0) as u8);
            e.0.extend_from_slice(&ratio.to_le_bytes());
        }
        None => e.u8(0),
    }
}

fn dec_probe(d: &mut Dec) -> R<Probe> {
    let uuid = |d: &mut Dec| -> R<[u8; 16]> {
        let mut u = [0u8; 16];
        for b in &mut u {
            *b = d.u8()?;
        }
        Ok(u)
    };
    let subvol = match d.u8()? {
        0 => None,
        1 => Some(Subvol {
            id: d.var()?,
            uuid: uuid(d)?,
            parent_uuid: match d.u8()? {
                0 => None,
                1 => Some(uuid(d)?),
                _ => return bad("parent uuid"),
            },
            read_only: d.bool()?,
            otime: d.zig()?,
            qgroup: match d.u8()? {
                0 => None,
                1 => Some((d.var()?, d.var()?)),
                _ => return bad("qgroup"),
            },
        }),
        _ => return bad("subvolume"),
    };
    let shares_extents = d.bool()?;
    let profile = match d.u8()? {
        0 => None,
        1 => {
            let name = *PROFILES
                .get(d.u8()? as usize)
                .ok_or(WireError::Malformed("profile"))?;
            let mut f = [0u8; 8];
            for b in &mut f {
                *b = d.u8()?;
            }
            let ratio = f64::from_le_bytes(f);
            if !(ratio.is_finite() && ratio > 0.0) {
                return bad("profile ratio");
            }
            Some((name, ratio))
        }
        _ => return bad("profile"),
    };
    Ok(Probe {
        subvol,
        shares_extents,
        profile,
    })
}

fn enc_chunk(e: &mut Enc, c: &Chunk) {
    e.var(c.job.0.into());
    e.var(c.dir.0.into());
    e.var(c.seq.into());
    match &c.root_meta {
        Some(m) => {
            e.u8(1);
            enc_meta(e, m);
        }
        None => e.u8(0),
    }
    match &c.probe {
        Some(p) => {
            e.u8(1);
            enc_probe(e, p);
        }
        None => e.u8(0),
    }
    e.bytes(&c.names);
    e.var(c.files.len() as u64);
    for f in &c.files {
        enc_span(e, f.name);
        enc_meta(e, &f.meta);
    }
    e.var(c.first_subdir.0.into());
    e.var(c.subdirs.len() as u64);
    for s in &c.subdirs {
        enc_span(e, s.name);
        enc_meta(e, &s.meta);
        e.bool(s.mount_root);
        e.u8(match s.skip {
            None => 0,
            Some(SkipReason::OtherFilesystem) => 1,
            Some(SkipReason::Network) => 2,
            Some(SkipReason::Kernel) => 3,
            Some(SkipReason::DuplicateMount) => 4,
            Some(SkipReason::Excluded) => 5,
        });
    }
    e.var(c.errors.len() as u64);
    for x in &c.errors {
        enc_span(e, x.name);
        enc_err(e, &x.err);
    }
    e.var(c.marks.len() as u64);
    for (entry, mark) in &c.marks {
        match *entry {
            Entry::File(i) => {
                e.u8(0);
                e.var(i.into());
            }
            Entry::Subdir(i) => {
                e.u8(1);
                e.var(i.into());
            }
        }
        match mark {
            Mark::Class(c) => {
                e.u8(0);
                enc_class(e, c);
            }
            Mark::Protected => e.u8(1),
            Mark::Excepted => e.u8(2),
        }
    }
    match &c.status {
        ChunkStatus::Ok => e.u8(0),
        ChunkStatus::Panicked(s) => {
            e.u8(1);
            e.bytes(s.as_bytes());
        }
        ChunkStatus::Cancelled => e.u8(2),
    }
}

/// Meta takes at least 8 bytes on the wire, a name span 2.
const MIN_META: usize = 8;

fn dec_chunk(d: &mut Dec, rules: Option<&RuleSet>) -> R<Chunk> {
    let job = JobId(d.u32()?);
    let dir = DirId(d.u32()?);
    let seq = d.u32()?;
    let root_meta = match d.u8()? {
        0 => None,
        1 => Some(dec_meta(d)?),
        _ => return bad("root meta"),
    };
    let probe = match d.u8()? {
        0 => None,
        1 => Some(Box::new(dec_probe(d)?)),
        _ => return bad("probe"),
    };
    let names = d.bytes()?.to_vec();
    let n = d.count(2 + MIN_META)?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        files.push(FileRec {
            name: dec_span(d, names.len())?,
            meta: dec_meta(d)?,
        });
    }
    let first_subdir = DirId(d.u32()?);
    let n = d.count(4 + MIN_META)?;
    if u64::from(first_subdir.0) + n as u64 > u64::from(u32::MAX) {
        return bad("subdirectory ids overflow");
    }
    let mut subdirs = Vec::with_capacity(n);
    for _ in 0..n {
        subdirs.push(SubdirRec {
            name: dec_span(d, names.len())?,
            meta: dec_meta(d)?,
            mount_root: d.bool()?,
            skip: match d.u8()? {
                0 => None,
                1 => Some(SkipReason::OtherFilesystem),
                2 => Some(SkipReason::Network),
                3 => Some(SkipReason::Kernel),
                4 => Some(SkipReason::DuplicateMount),
                5 => Some(SkipReason::Excluded),
                _ => return bad("skip reason"),
            },
        });
    }
    let n = d.count(3)?;
    let mut errors = Vec::with_capacity(n);
    for _ in 0..n {
        errors.push(EntryError {
            name: dec_span(d, names.len())?,
            err: dec_err(d)?,
        });
    }
    let n = d.count(3)?;
    let mut marks = Vec::with_capacity(n);
    for _ in 0..n {
        let entry = match d.u8()? {
            0 => in_chunk(Entry::File(d.u32()?), files.len())?,
            1 => in_chunk(Entry::Subdir(d.u32()?), subdirs.len())?,
            _ => return bad("entry"),
        };
        let mark = match d.u8()? {
            0 => Mark::Class(dec_class(d, rules)?),
            1 => Mark::Protected,
            2 => Mark::Excepted,
            _ => return bad("mark"),
        };
        marks.push((entry, mark));
    }
    let status = match d.u8()? {
        0 => ChunkStatus::Ok,
        1 => ChunkStatus::Panicked(String::from_utf8_lossy(d.bytes()?).into_owned()),
        2 => ChunkStatus::Cancelled,
        _ => return bad("chunk status"),
    };
    Ok(Chunk {
        job,
        dir,
        seq,
        root_meta,
        probe,
        names,
        files,
        first_subdir,
        subdirs,
        errors,
        marks,
        status,
    })
}

/// `e`, if it is one of `len` entries of its kind.
fn in_chunk(e: Entry, len: usize) -> R<Entry> {
    let (Entry::File(i) | Entry::Subdir(i)) = e;
    if (i as usize) < len {
        Ok(e)
    } else {
        bad("mark of an entry not in the chunk")
    }
}

fn enc_msg(e: &mut Enc, m: &ScanMsg) {
    match m {
        ScanMsg::Chunk(c) => {
            e.u8(0);
            enc_chunk(e, c);
        }
        ScanMsg::Close {
            job,
            dir,
            chunks,
            status,
            classes,
        } => {
            e.u8(1);
            e.var(job.0.into());
            e.var(dir.0.into());
            e.var((*chunks).into());
            match status {
                ListingStatus::Complete => e.u8(0),
                ListingStatus::Partial {
                    panicked,
                    cancelled,
                    read_error,
                } => {
                    e.u8(1);
                    e.var((*panicked).into());
                    e.var((*cancelled).into());
                    match read_error {
                        Some(k) => {
                            e.u8(1);
                            enc_err(e, k);
                        }
                        None => e.u8(0),
                    }
                }
                ListingStatus::Failed(k) => {
                    e.u8(2);
                    enc_err(e, k);
                }
                ListingStatus::Cancelled => e.u8(3),
            }
            e.var(classes.len() as u64);
            for (d, c) in classes {
                e.var(d.0.into());
                enc_class(e, c);
            }
        }
        ScanMsg::Stalled { job, dir } => {
            e.u8(2);
            e.var(job.0.into());
            e.var(dir.0.into());
        }
        ScanMsg::Quiescent { submitted_seq } => {
            e.u8(3);
            e.var(*submitted_seq);
        }
    }
}

fn dec_msg(d: &mut Dec, rules: Option<&RuleSet>) -> R<ScanMsg> {
    Ok(match d.u8()? {
        0 => ScanMsg::Chunk(dec_chunk(d, rules)?),
        1 => {
            let job = JobId(d.u32()?);
            let dir = DirId(d.u32()?);
            let chunks = d.u32()?;
            let status = match d.u8()? {
                0 => ListingStatus::Complete,
                1 => ListingStatus::Partial {
                    panicked: d.u32()?,
                    cancelled: d.u32()?,
                    read_error: match d.u8()? {
                        0 => None,
                        1 => Some(dec_err(d)?),
                        _ => return bad("read error"),
                    },
                },
                2 => ListingStatus::Failed(dec_err(d)?),
                3 => ListingStatus::Cancelled,
                _ => return bad("listing status"),
            };
            let n = d.count(5)?;
            let mut classes = Vec::with_capacity(n);
            for _ in 0..n {
                classes.push((DirId(d.u32()?), dec_class(d, rules)?));
            }
            ScanMsg::Close {
                job,
                dir,
                chunks,
                status,
                classes,
            }
        }
        2 => ScanMsg::Stalled {
            job: JobId(d.u32()?),
            dir: DirId(d.u32()?),
        },
        3 => ScanMsg::Quiescent {
            submitted_seq: d.var()?,
        },
        _ => return bad("scanner message"),
    })
}

impl FromAgent {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            FromAgent::Hello { min, max, tool } => {
                e.u8(0);
                e.var((*min).into());
                e.var((*max).into());
                e.bytes(tool.as_bytes());
            }
            FromAgent::Batch(msgs) => {
                e.u8(1);
                e.var(msgs.len() as u64);
                for m in msgs {
                    enc_msg(&mut e, m);
                }
            }
            FromAgent::Heartbeat => e.u8(2),
            FromAgent::Rules {
                platform,
                vars,
                fingerprint,
                root,
            } => {
                e.u8(6);
                e.bytes(platform.as_bytes());
                e.var(vars.len() as u64);
                for (k, v) in vars {
                    e.bytes(k.as_bytes());
                    e.bytes(v.as_bytes());
                }
                e.var(*fingerprint);
                enc_wclass(&mut e, root);
            }
            FromAgent::Started { job, seq } => {
                e.u8(3);
                e.var(job.0.into());
                e.var(*seq);
            }
            FromAgent::Abandoned { job, dir, complete } => {
                e.u8(4);
                e.var(job.0.into());
                e.var(dir.0.into());
                e.bool(*complete);
            }
            FromAgent::Fatal(s) => {
                e.u8(5);
                e.bytes(s.as_bytes());
            }
            FromAgent::Deleter(m) => {
                e.u8(7);
                enc_del_msg(&mut e, m);
            }
            FromAgent::Checked(msgs) => {
                e.u8(8);
                e.var(msgs.len() as u64);
                for m in msgs {
                    enc_check_msg(&mut e, m);
                }
            }
            FromAgent::Host {
                measure_min,
                privileged,
            } => {
                e.u8(9);
                e.var(*measure_min);
                e.bool(*privileged);
            }
            FromAgent::Measured(msgs) => {
                e.u8(10);
                e.var(msgs.len() as u64);
                for m in msgs {
                    enc_measure_msg(&mut e, m);
                }
            }
        }
        e.0
    }

    /// Decodes a payload; classes must name one of the local side's `rules`, and there are
    /// none without them. [`FromAgent::Rules`]'s root is not checked: the local side checks it
    /// against the set it then builds.
    pub fn decode(buf: &[u8], rules: Option<&RuleSet>) -> R<FromAgent> {
        let mut d = Dec::new(buf);
        let up = match d.u8()? {
            0 => FromAgent::Hello {
                min: d.u32()?,
                max: d.u32()?,
                tool: String::from_utf8_lossy(d.bytes()?).into_owned(),
            },
            1 => {
                let n = d.count(2)?;
                let mut msgs = Vec::with_capacity(n);
                for _ in 0..n {
                    msgs.push(dec_msg(&mut d, rules)?);
                }
                FromAgent::Batch(msgs)
            }
            2 => FromAgent::Heartbeat,
            3 => FromAgent::Started {
                job: JobId(d.u32()?),
                seq: d.var()?,
            },
            4 => FromAgent::Abandoned {
                job: JobId(d.u32()?),
                dir: DirId(d.u32()?),
                complete: d.bool()?,
            },
            5 => FromAgent::Fatal(String::from_utf8_lossy(d.bytes()?).into_owned()),
            7 => FromAgent::Deleter(dec_del_msg(&mut d)?),
            8 => {
                let n = d.count(3)?;
                let mut msgs = Vec::with_capacity(n);
                for _ in 0..n {
                    msgs.push(dec_check_msg(&mut d)?);
                }
                FromAgent::Checked(msgs)
            }
            9 => FromAgent::Host {
                measure_min: d.var()?,
                privileged: d.bool()?,
            },
            10 => {
                let n = d.count(4)?;
                let mut msgs = Vec::with_capacity(n);
                for _ in 0..n {
                    msgs.push(dec_measure_msg(&mut d)?);
                }
                FromAgent::Measured(msgs)
            }
            6 => {
                let platform = d.text()?;
                let n = d.count(2)?;
                let mut vars = Vec::with_capacity(n);
                for _ in 0..n {
                    vars.push((d.text()?, d.text()?));
                }
                FromAgent::Rules {
                    platform,
                    vars,
                    fingerprint: d.var()?,
                    root: dec_wclass(&mut d, None)?,
                }
            }
            _ => return bad("message tag"),
        };
        d.end()?;
        Ok(up)
    }
}

impl ToAgent {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            ToAgent::Hello { version } => {
                e.u8(0);
                e.var((*version).into());
            }
            ToAgent::Rules { files, root } => {
                e.u8(5);
                e.var(files.len() as u64);
                for (name, text) in files {
                    match name {
                        Some(n) => {
                            e.u8(1);
                            e.bytes(n.as_bytes());
                        }
                        None => e.u8(0),
                    }
                    e.bytes(text.as_bytes());
                }
                e.bytes(root);
            }
            ToAgent::StartJob {
                job,
                root,
                cross_mounts,
                network,
                class,
            } => {
                e.u8(1);
                e.var(job.0.into());
                e.bytes(root);
                e.bool(*cross_mounts);
                e.bool(*network);
                match class {
                    Some(c) => {
                        e.u8(1);
                        enc_wclass(&mut e, c);
                    }
                    None => e.u8(0),
                }
            }
            ToAgent::EndJob { job } => {
                e.u8(2);
                e.var(job.0.into());
            }
            ToAgent::StopDirs { job, dirs } => {
                e.u8(3);
                e.var(job.0.into());
                e.var(dirs.len() as u64);
                for d in dirs {
                    e.var(d.0.into());
                }
            }
            ToAgent::Abandon { job, dir } => {
                e.u8(4);
                e.var(job.0.into());
                e.var(dir.0.into());
            }
            ToAgent::Delete(r) => {
                e.u8(6);
                enc_del_req(&mut e, r);
            }
            ToAgent::AbortDelete => e.u8(7),
            ToAgent::Check(reqs) => {
                e.u8(8);
                e.var(reqs.len() as u64);
                for r in reqs {
                    enc_check_req(&mut e, r);
                }
            }
            ToAgent::Measure(reqs) => {
                e.u8(9);
                e.var(reqs.len() as u64);
                for r in reqs {
                    enc_measure_req(&mut e, r);
                }
            }
        }
        e.0
    }

    /// Decodes a payload; a job's class must name the agent's `rules`, and there is none
    /// without them.
    pub fn decode(buf: &[u8], rules: Option<&RuleSet>) -> R<ToAgent> {
        let mut d = Dec::new(buf);
        let down = match d.u8()? {
            0 => ToAgent::Hello { version: d.u32()? },
            1 => ToAgent::StartJob {
                job: JobId(d.u32()?),
                root: d.bytes()?.to_vec(),
                cross_mounts: d.bool()?,
                network: d.bool()?,
                class: match d.u8()? {
                    0 => None,
                    1 if rules.is_some() => Some(dec_wclass(&mut d, rules)?),
                    1 => return bad("a class without rules"),
                    _ => return bad("class"),
                },
            },
            2 => ToAgent::EndJob {
                job: JobId(d.u32()?),
            },
            3 => {
                let job = JobId(d.u32()?);
                let n = d.count(1)?;
                let mut dirs = Vec::with_capacity(n);
                for _ in 0..n {
                    dirs.push(DirId(d.u32()?));
                }
                ToAgent::StopDirs { job, dirs }
            }
            4 => ToAgent::Abandon {
                job: JobId(d.u32()?),
                dir: DirId(d.u32()?),
            },
            6 => ToAgent::Delete(dec_del_req(&mut d, rules)?),
            7 => ToAgent::AbortDelete,
            8 => {
                let n = d.count(2)?;
                let mut reqs = Vec::with_capacity(n);
                for _ in 0..n {
                    reqs.push(dec_check_req(&mut d, rules)?);
                }
                ToAgent::Check(reqs)
            }
            9 => {
                let n = d.count(3)?;
                let mut reqs = Vec::with_capacity(n);
                for _ in 0..n {
                    reqs.push(dec_measure_req(&mut d)?);
                }
                ToAgent::Measure(reqs)
            }
            5 => {
                let n = d.count(2)?;
                let mut files = Vec::with_capacity(n);
                for _ in 0..n {
                    let name = match d.u8()? {
                        0 => None,
                        1 => Some(d.text()?),
                        _ => return bad("rule file name"),
                    };
                    files.push((name, d.text()?));
                }
                ToAgent::Rules {
                    files,
                    root: d.bytes()?.to_vec(),
                }
            }
            _ => return bad("message tag"),
        };
        d.end()?;
        Ok(down)
    }
}

fn enc_check_req(e: &mut Enc, r: &CheckReq) {
    match r {
        CheckReq::Guards {
            node,
            rule,
            path,
            vcs,
            processes,
        } => {
            e.u8(0);
            e.var(node.to_bits());
            e.rule(*rule);
            enc_path(e, path);
            e.bool(*vcs);
            e.var(processes.len() as u64);
            for p in processes {
                e.bytes(p.as_bytes());
            }
        }
        // The agent builds the command from its own rules.
        CheckReq::NativeSize { rule, .. } => {
            e.u8(1);
            e.rule(*rule);
        }
    }
}

fn dec_check_req(d: &mut Dec, rules: Option<&RuleSet>) -> R<CheckReq> {
    match d.u8()? {
        0 => {
            let node = NodeRef::from_bits(d.var()?);
            let rule = d.rule()?;
            let path = dec_path(d)?;
            let vcs = d.bool()?;
            let n = d.count(1)?;
            let mut processes = Vec::with_capacity(n);
            for _ in 0..n {
                processes.push(d.lossy()?);
            }
            Ok(CheckReq::Guards {
                node,
                rule,
                path,
                vcs,
                processes,
            })
        }
        1 => {
            let rule = d.rule()?;
            match rules
                .and_then(|r| r.rules().get(usize::from(rule.0)))
                .and_then(|r| r.native.as_ref())
                .filter(|n| !n.size_command.is_empty())
            {
                Some(n) => Ok(CheckReq::NativeSize {
                    rule,
                    check: n.check.clone(),
                    command: n.size_command.clone(),
                }),
                None => bad("a native size of a rule without one"),
            }
        }
        _ => bad("check request"),
    }
}

fn enc_check_msg(e: &mut Enc, m: &CheckMsg) {
    match m {
        CheckMsg::Guards {
            node,
            rule,
            tracked,
            running,
        } => {
            e.u8(0);
            e.var(node.to_bits());
            e.rule(*rule);
            e.u8(match tracked {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            });
            match running {
                Some(p) => {
                    e.u8(1);
                    e.bytes(p.as_bytes());
                }
                None => e.u8(0),
            }
        }
        CheckMsg::NativeSize { rule, result } => {
            e.u8(1);
            e.rule(*rule);
            enc_result(e, result, |e, n| e.var(*n));
        }
    }
}

fn dec_check_msg(d: &mut Dec) -> R<CheckMsg> {
    Ok(match d.u8()? {
        0 => CheckMsg::Guards {
            node: NodeRef::from_bits(d.var()?),
            rule: d.rule()?,
            tracked: match d.u8()? {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return bad("tracked"),
            },
            running: match d.u8()? {
                0 => None,
                1 => Some(d.lossy()?),
                _ => return bad("running"),
            },
        },
        1 => CheckMsg::NativeSize {
            rule: d.rule()?,
            result: dec_result(d, |d| d.var())?,
        },
        _ => return bad("check answer"),
    })
}

fn enc_measure_req(e: &mut Enc, r: &MeasureReq) {
    e.var(r.round);
    enc_path(e, &r.dir);
    e.var(r.files.len() as u64);
    for f in &r.files {
        e.var(f.node.to_bits());
        e.bytes(&f.name);
        e.var(f.inode.0);
        e.var(f.inode.1);
    }
}

fn dec_measure_req(d: &mut Dec) -> R<MeasureReq> {
    let round = d.var()?;
    let dir = dec_path(d)?;
    let n = d.count(4)?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        let node = NodeRef::from_bits(d.var()?);
        let name = d.bytes()?;
        // A name, not a path: the measurer opens it relative to `dir`.
        if name.is_empty() || name.contains(&b'/') || name == b"." || name == b".." {
            return bad("file name");
        }
        files.push(MeasureFile {
            node,
            name: name.to_vec(),
            inode: (d.var()?, d.var()?),
        });
    }
    Ok(MeasureReq { round, dir, files })
}

fn enc_measure_msg(e: &mut Enc, m: &MeasureMsg) {
    e.var(m.round);
    e.var(m.node.to_bits());
    e.var(m.inode.0);
    e.var(m.inode.1);
    match &m.result {
        MeasureResult::Measured(me) => {
            e.u8(0);
            e.bytes(&me.fs);
            e.var(me.excl);
            e.bool(me.compressed);
            e.bool(me.exact);
            e.var(me.ranges.len() as u64);
            for &(a, b) in &me.ranges {
                e.var(a);
                e.var(b);
            }
            e.var(me.points.len() as u64);
            for &(a, n) in &me.points {
                e.var(a);
                e.var(n);
            }
        }
        MeasureResult::Changed => e.u8(1),
        MeasureResult::Failed(k) => {
            e.u8(2);
            enc_err(e, k);
        }
    }
}

fn dec_measure_msg(d: &mut Dec) -> R<MeasureMsg> {
    let round = d.var()?;
    let node = NodeRef::from_bits(d.var()?);
    let inode = (d.var()?, d.var()?);
    let result = match d.u8()? {
        0 => {
            let fs: [u8; 16] = d.bytes()?.try_into().or(bad("filesystem id"))?;
            let excl = d.var()?;
            let compressed = d.bool()?;
            let exact = d.bool()?;
            let n = d.count(2)?;
            let mut ranges = Vec::with_capacity(n);
            for _ in 0..n {
                let (a, b) = (d.var()?, d.var()?);
                if a > b {
                    return bad("extent range");
                }
                ranges.push((a, b));
            }
            let n = d.count(2)?;
            let mut points = Vec::with_capacity(n);
            for _ in 0..n {
                points.push((d.var()?, d.var()?));
            }
            MeasureResult::Measured(Measurement {
                fs,
                excl,
                compressed,
                ranges,
                points,
                exact,
            })
        }
        1 => MeasureResult::Changed,
        2 => MeasureResult::Failed(dec_err(d)?),
        _ => return bad("measure result"),
    };
    Ok(MeasureMsg {
        round,
        node,
        inode,
        result,
    })
}

fn enc_path(e: &mut Enc, p: &std::path::Path) {
    e.bytes(&dfdu_tree::name_bytes(p.as_os_str()));
}

fn dec_path(d: &mut Dec) -> R<PathBuf> {
    Ok(PathBuf::from(dfdu_tree::name_os(d.bytes()?).into_owned()))
}

fn enc_kind(e: &mut Enc, k: Kind) {
    e.u8(match k {
        Kind::File => 0,
        Kind::Dir => 1,
        Kind::Symlink => 2,
        Kind::Other => 3,
    });
}

fn dec_kind(d: &mut Dec) -> R<Kind> {
    Ok(match d.u8()? {
        0 => Kind::File,
        1 => Kind::Dir,
        2 => Kind::Symlink,
        3 => Kind::Other,
        _ => return bad("kind"),
    })
}

fn enc_result<T>(e: &mut Enc, r: &Result<T, String>, ok: impl FnOnce(&mut Enc, &T)) {
    match r {
        Ok(v) => {
            e.u8(0);
            ok(e, v);
        }
        Err(s) => {
            e.u8(1);
            e.bytes(s.as_bytes());
        }
    }
}

fn dec_result<T>(d: &mut Dec, ok: impl FnOnce(&mut Dec) -> R<T>) -> R<Result<T, String>> {
    Ok(match d.u8()? {
        0 => Ok(ok(d)?),
        1 => Err(d.lossy()?),
        _ => return bad("result"),
    })
}

fn enc_del_req(e: &mut Enc, r: &DeleterReq) {
    match r {
        DeleterReq::Plan(p) => {
            e.u8(0);
            e.var(p.id);
            e.u8(match p.mode {
                Mode::Trash => 0,
                Mode::Stage => 1,
                Mode::Permanent => 2,
            });
            enc_path(e, &p.scan_root);
            e.var(p.items.len() as u64);
            for it in &p.items {
                e.var(it.node.to_bits());
                enc_path(e, &it.path);
                let id = &it.identity;
                e.var(id.dev);
                e.var(id.ino);
                enc_kind(e, id.kind);
                e.var(id.apparent);
                e.zig(id.mtime);
                e.var(it.bytes);
                // Sorted, so a plan always encodes alike.
                let mut dirs: Vec<_> = it.inside.iter().collect();
                dirs.sort_by(|a, b| a.0.cmp(b.0));
                e.var(dirs.len() as u64);
                for (rel, l) in dirs {
                    enc_path(e, rel);
                    e.var(l.ino);
                    e.var(l.entries.len() as u64);
                    for &(h, ino, k) in &l.entries {
                        e.0.extend_from_slice(&h.to_le_bytes());
                        e.var(ino);
                        enc_kind(e, k);
                    }
                }
            }
        }
        DeleterReq::Undo { plan } => {
            e.u8(1);
            e.var(*plan);
        }
        DeleterReq::Commit => e.u8(2),
        // The agent builds the command from its own rules; one built from none is refused.
        DeleterReq::Native(n) => match &n.rule {
            Some(r) => {
                e.u8(3);
                e.var(n.id);
                e.rule(r.rule);
                match r.variant {
                    None => e.u8(0),
                    Some(v) => {
                        e.u8(1);
                        e.var(v.into());
                    }
                }
                enc_path(e, &r.path);
                e.bool(n.dry_run);
            }
            None => e.u8(4),
        },
    }
}

fn dec_del_req(d: &mut Dec, rules: Option<&RuleSet>) -> R<DeleterReq> {
    Ok(match d.u8()? {
        0 => {
            let id = d.var()?;
            let mode = match d.u8()? {
                0 => Mode::Trash,
                1 => Mode::Stage,
                2 => Mode::Permanent,
                _ => return bad("delete mode"),
            };
            let scan_root = dec_path(d)?;
            let n = d.count(8)?;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                let node = NodeRef::from_bits(d.var()?);
                let path = dec_path(d)?;
                let identity = Identity {
                    dev: d.var()?,
                    ino: d.var()?,
                    kind: dec_kind(d)?,
                    apparent: d.var()?,
                    mtime: d.zig()?,
                };
                let bytes = d.var()?;
                let dirs = d.count(3)?;
                let mut inside = Listings::with_capacity(dirs);
                for _ in 0..dirs {
                    let rel = dec_path(d)?;
                    let ino = d.var()?;
                    let m = d.count(10)?;
                    let mut entries = Vec::with_capacity(m);
                    for _ in 0..m {
                        let h = d.u64_le()?;
                        entries.push((h, d.var()?, dec_kind(d)?));
                    }
                    if inside.insert(rel, Listing { ino, entries }).is_some() {
                        return bad("a directory listed twice");
                    }
                }
                items.push(PlanItem {
                    node,
                    path,
                    identity,
                    bytes,
                    inside: Arc::new(inside),
                });
            }
            DeleterReq::Plan(Plan {
                id,
                mode,
                scan_root,
                items,
            })
        }
        1 => DeleterReq::Undo { plan: d.var()? },
        2 => DeleterReq::Commit,
        3 => {
            let id = d.var()?;
            let rule = d.rule()?;
            let variant = match d.u8()? {
                0 => None,
                1 => Some(d.u16()?),
                _ => return bad("native variant"),
            };
            let path = dec_path(d)?;
            if !path.is_absolute() {
                return bad("a native command for a relative path");
            }
            let dry_run = d.bool()?;
            match rules
                .and_then(|r| crate::applier::native_req(r, rule, variant, &path, dry_run, id))
            {
                Some(req) => DeleterReq::Native(req),
                None => return bad("a native command of a rule without one"),
            }
        }
        _ => return bad("deleter request"),
    })
}

fn enc_refusal(e: &mut Enc, r: &Refusal) {
    match r {
        Refusal::Protected => e.u8(0),
        Refusal::OutsideScanRoot => e.u8(1),
        Refusal::ScanRoot => e.u8(2),
        Refusal::MountPoint => e.u8(3),
        Refusal::ContainsMount(p) => {
            e.u8(4);
            enc_path(e, p);
        }
        Refusal::ReadOnly => e.u8(5),
        Refusal::TrashCrossesFilesystems => e.u8(6),
        Refusal::TrashOtherSubvolume(p) => {
            e.u8(7);
            enc_path(e, p);
        }
        Refusal::NoJournal => e.u8(8),
    }
}

fn dec_refusal(d: &mut Dec) -> R<Refusal> {
    Ok(match d.u8()? {
        0 => Refusal::Protected,
        1 => Refusal::OutsideScanRoot,
        2 => Refusal::ScanRoot,
        3 => Refusal::MountPoint,
        4 => Refusal::ContainsMount(dec_path(d)?),
        5 => Refusal::ReadOnly,
        6 => Refusal::TrashCrossesFilesystems,
        7 => Refusal::TrashOtherSubvolume(dec_path(d)?),
        8 => Refusal::NoJournal,
        _ => return bad("refusal"),
    })
}

fn enc_del_msg(e: &mut Enc, m: &DeleterMsg) {
    let texts = |e: &mut Enc, v: &[String]| {
        e.var(v.len() as u64);
        for s in v {
            e.bytes(s.as_bytes());
        }
    };
    match m {
        DeleterMsg::Item {
            plan,
            node,
            bytes,
            result,
        } => {
            e.u8(0);
            e.var(*plan);
            e.var(node.to_bits());
            e.var(*bytes);
            match result {
                DeleteResult::Deleted => e.u8(0),
                DeleteResult::Stale(s) => {
                    e.u8(1);
                    e.bytes(s.as_bytes());
                }
                DeleteResult::Refused(r) => {
                    e.u8(2);
                    enc_refusal(e, r);
                }
                DeleteResult::Failed { partial, err, msg } => {
                    e.u8(3);
                    e.bool(*partial);
                    enc_err(e, err);
                    e.bytes(msg.as_bytes());
                }
                DeleteResult::Aborted => e.u8(4),
            }
        }
        DeleterMsg::Done { plan, btrfs } => {
            e.u8(1);
            e.var(*plan);
            e.bool(*btrfs);
        }
        DeleterMsg::Committed {
            plan,
            bytes,
            result,
        } => {
            e.u8(2);
            e.var(*plan);
            e.var(*bytes);
            enc_result(e, result, |_, ()| {});
        }
        DeleterMsg::Restored {
            plan,
            original,
            staged,
            bytes,
            result,
        } => {
            e.u8(3);
            e.var(*plan);
            enc_path(e, original);
            e.bool(*staged);
            e.var(*bytes);
            enc_result(e, result, |e, p| enc_path(e, p));
        }
        DeleterMsg::UndoDone { plan } => {
            e.u8(4);
            e.var(*plan);
        }
        DeleterMsg::NativeOutput { id, line } => {
            e.u8(5);
            e.var(*id);
            e.bytes(line.as_bytes());
        }
        DeleterMsg::NativeDone { id, result } => {
            e.u8(6);
            e.var(*id);
            enc_result(e, result, |_, ()| {});
        }
        DeleterMsg::Unfinished { items, notes } => {
            e.u8(7);
            texts(e, items);
            texts(e, notes);
        }
    }
}

fn dec_del_msg(d: &mut Dec) -> R<DeleterMsg> {
    let texts = |d: &mut Dec| -> R<Vec<String>> {
        let n = d.count(1)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(d.lossy()?);
        }
        Ok(v)
    };
    Ok(match d.u8()? {
        0 => DeleterMsg::Item {
            plan: d.var()?,
            node: NodeRef::from_bits(d.var()?),
            bytes: d.var()?,
            result: match d.u8()? {
                0 => DeleteResult::Deleted,
                1 => DeleteResult::Stale(d.lossy()?),
                2 => DeleteResult::Refused(dec_refusal(d)?),
                3 => DeleteResult::Failed {
                    partial: d.bool()?,
                    err: dec_err(d)?,
                    msg: d.lossy()?,
                },
                4 => DeleteResult::Aborted,
                _ => return bad("delete result"),
            },
        },
        1 => DeleterMsg::Done {
            plan: d.var()?,
            btrfs: d.bool()?,
        },
        2 => DeleterMsg::Committed {
            plan: d.var()?,
            bytes: d.var()?,
            result: dec_result(d, |_| Ok(()))?,
        },
        3 => DeleterMsg::Restored {
            plan: d.var()?,
            original: dec_path(d)?,
            staged: d.bool()?,
            bytes: d.var()?,
            result: dec_result(d, dec_path)?,
        },
        4 => DeleterMsg::UndoDone { plan: d.var()? },
        5 => DeleterMsg::NativeOutput {
            id: d.var()?,
            line: d.lossy()?,
        },
        6 => DeleterMsg::NativeDone {
            id: d.var()?,
            result: dec_result(d, |_| Ok(()))?,
        },
        7 => DeleterMsg::Unfinished {
            items: texts(d)?,
            notes: texts(d)?,
        },
        _ => return bad("deleter message"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in rules for Linux, and the number of one with a `degrade` condition.
    fn few_rules() -> RuleSet {
        dfdu_rules::load_from(
            &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
            &[],
        )
        .0
    }

    fn rules() -> (RuleSet, RuleIx) {
        let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
        let set = dfdu_rules::builtin(&env).0;
        let nm = set.index("node.node_modules").or_else(|| {
            let i = set.rules().iter().position(|r| !r.degrade.is_empty())?;
            Some(RuleIx(i as u16))
        });
        (set, nm.unwrap())
    }

    fn meta(kind: Kind, n: u64) -> Meta {
        Meta {
            kind,
            apparent: n,
            alloc: n.wrapping_mul(4096),
            mtime: (n as i64).wrapping_mul(-1_000_003),
            dev: 7,
            ino: n << 40,
            nlink: 3,
            mount: 0,
        }
    }

    fn sample() -> Vec<FromAgent> {
        let rule = rules().1;
        let names = b"one\xfftwosub".to_vec();
        let chunk = Chunk {
            job: JobId(2),
            dir: DirId(9),
            seq: 70,
            root_meta: Some(meta(Kind::Dir, 1)),
            probe: Some(Box::new(Probe {
                subvol: Some(Subvol {
                    id: 256,
                    uuid: [3; 16],
                    parent_uuid: Some([4; 16]),
                    read_only: true,
                    otime: -5,
                    qgroup: Some((1 << 33, 12)),
                }),
                shares_extents: true,
                profile: Some(("RAID1C3", 3.0)),
            })),
            names,
            files: vec![
                FileRec {
                    name: NameSpan { off: 0, len: 4 },
                    meta: meta(Kind::File, 10),
                },
                FileRec {
                    name: NameSpan { off: 4, len: 3 },
                    meta: meta(Kind::Symlink, u64::MAX),
                },
            ],
            first_subdir: DirId(40),
            subdirs: vec![SubdirRec {
                name: NameSpan { off: 7, len: 3 },
                meta: meta(Kind::Dir, 2),
                mount_root: true,
                skip: Some(SkipReason::DuplicateMount),
            }],
            errors: vec![EntryError {
                name: NameSpan { off: 0, len: 0 },
                err: ErrKind::Os(-22),
            }],
            marks: vec![
                (
                    Entry::Subdir(0),
                    Mark::Class(Class {
                        rule,
                        safety: Safety::Review,
                        degraded: Some(0),
                        flags: 0,
                    }),
                ),
                (Entry::File(1), Mark::Excepted),
            ],
            status: ChunkStatus::Panicked("boom".into()),
        };
        vec![
            FromAgent::Hello {
                min: 1,
                max: 1,
                tool: "dfdu 0.1.0".into(),
            },
            FromAgent::Batch(vec![
                ScanMsg::Chunk(chunk),
                ScanMsg::Close {
                    job: JobId(2),
                    dir: DirId(9),
                    chunks: 1,
                    status: ListingStatus::Partial {
                        panicked: 1,
                        cancelled: 0,
                        read_error: Some(ErrKind::PermissionDenied),
                    },
                    classes: vec![(
                        DirId(40),
                        Class {
                            rule: RuleIx(0),
                            safety: Safety::Regenerable,
                            degraded: None,
                            flags: 0,
                        },
                    )],
                },
                ScanMsg::Stalled {
                    job: JobId(2),
                    dir: DirId(40),
                },
                ScanMsg::Quiescent {
                    submitted_seq: u64::MAX,
                },
            ]),
            FromAgent::Heartbeat,
            FromAgent::Started {
                job: JobId(2),
                seq: 17,
            },
            FromAgent::Abandoned {
                job: JobId(2),
                dir: DirId(5),
                complete: false,
            },
            FromAgent::Fatal("gone".into()),
            FromAgent::Deleter(DeleterMsg::Item {
                plan: 3,
                node: NodeRef::from_bits(u64::MAX - 9),
                bytes: 4096,
                result: DeleteResult::Failed {
                    partial: true,
                    err: ErrKind::Os(13),
                    msg: "denied".into(),
                },
            }),
            FromAgent::Deleter(DeleterMsg::Item {
                plan: 3,
                node: NodeRef::from_bits(1),
                bytes: 0,
                result: DeleteResult::Refused(Refusal::TrashOtherSubvolume("/sv".into())),
            }),
            FromAgent::Deleter(DeleterMsg::Done {
                plan: 3,
                btrfs: true,
            }),
            FromAgent::Deleter(DeleterMsg::Restored {
                plan: 3,
                original: "/a/\u{e9}".into(),
                staged: true,
                bytes: 9,
                result: Ok("/a/\u{e9}.restored-1".into()),
            }),
            FromAgent::Deleter(DeleterMsg::Committed {
                plan: 3,
                bytes: 9,
                result: Err("busy".into()),
            }),
            FromAgent::Checked(vec![
                CheckMsg::Guards {
                    node: NodeRef::from_bits(5),
                    rule,
                    tracked: Some(true),
                    running: Some("Xcode".into()),
                },
                CheckMsg::Guards {
                    node: NodeRef::from_bits(6),
                    rule,
                    tracked: None,
                    running: None,
                },
                CheckMsg::NativeSize {
                    rule,
                    result: Ok(1 << 40),
                },
            ]),
            FromAgent::Deleter(DeleterMsg::Unfinished {
                items: vec!["x".into()],
                notes: vec![],
            }),
            FromAgent::Rules {
                platform: "linux".into(),
                vars: vec![("HOME".into(), "/home/u".into())],
                fingerprint: u64::MAX - 3,
                root: class(rule),
            },
            FromAgent::Host {
                measure_min: 1 << 20,
                privileged: true,
            },
            FromAgent::Measured(vec![
                MeasureMsg {
                    round: 3,
                    node: NodeRef::from_bits(42 << 32 | 3),
                    inode: (7, u64::MAX),
                    result: MeasureResult::Measured(Measurement {
                        fs: [9; 16],
                        excl: 4096,
                        compressed: true,
                        ranges: vec![(0, 8192), (1 << 40, 1 << 41)],
                        points: vec![(77, 4096)],
                        exact: false,
                    }),
                },
                MeasureMsg {
                    round: 3,
                    node: NodeRef::from_bits(5),
                    inode: (7, 8),
                    result: MeasureResult::Changed,
                },
                MeasureMsg {
                    round: 0,
                    node: NodeRef::from_bits(6),
                    inode: (7, 9),
                    result: MeasureResult::Failed(ErrKind::Os(5)),
                },
            ]),
        ]
    }

    fn measure_req() -> MeasureReq {
        MeasureReq {
            round: 2,
            dir: "/srv/\u{e9}t\u{e9}".into(),
            files: vec![
                MeasureFile {
                    node: NodeRef::from_bits(42 << 32 | 3),
                    name: b"a\xffb".to_vec(),
                    inode: (1, 2),
                },
                MeasureFile {
                    node: NodeRef::from_bits(0),
                    name: b"c".to_vec(),
                    inode: (u64::MAX, 0),
                },
            ],
        }
    }

    fn plan() -> Plan {
        let mut inside = Listings::default();
        inside.insert(
            "".into(),
            Listing {
                ino: 5,
                entries: vec![(u64::MAX, 0, Kind::Dir), (1, 77, Kind::File)],
            },
        );
        inside.insert("sub/\u{e9}".into(), Listing::default());
        Plan {
            id: 7,
            mode: Mode::Stage,
            scan_root: "/home/u".into(),
            items: vec![PlanItem {
                node: NodeRef::from_bits(42 << 32 | 3),
                path: "/home/u/cache".into(),
                identity: Identity {
                    dev: 1,
                    ino: 2,
                    kind: Kind::Dir,
                    apparent: 3,
                    mtime: -4,
                },
                bytes: 8192,
                inside: Arc::new(inside),
            }],
        }
    }

    fn class(rule: RuleIx) -> WireClass {
        WireClass {
            path: b"/home/u/\xffproj".to_vec(),
            inside: vec![RuleIx(0), rule],
            protection: State::Sticky,
            chain: vec![Hit {
                rule,
                safety: Safety::Review,
                degraded: Some(0),
                inside: Inside::Check,
                open: true,
            }],
        }
    }

    /// Every message reads back as written (compared by writing it again).
    #[test]
    fn messages_round_trip() {
        let (set, rule) = rules();
        for up in sample() {
            let bytes = up.encode();
            let back = FromAgent::decode(&bytes, Some(&set)).unwrap();
            assert_eq!(back.encode(), bytes, "{up:?}");
        }
        let downs = [
            ToAgent::Hello { version: 1 },
            ToAgent::Rules {
                files: vec![
                    (None, "[[rule]]".into()),
                    (Some("/home/u/.config/dfdu/rules/a.toml".into()), "".into()),
                ],
                root: b"/srv/\xffdata".to_vec(),
            },
            ToAgent::StartJob {
                job: JobId(1),
                root: b"/srv/\xffdata".to_vec(),
                cross_mounts: true,
                network: false,
                class: None,
            },
            ToAgent::StartJob {
                job: JobId(1),
                root: b"/".to_vec(),
                cross_mounts: false,
                network: true,
                class: Some(class(rule)),
            },
            ToAgent::Delete(DeleterReq::Plan(plan())),
            ToAgent::Delete(DeleterReq::Undo { plan: 7 }),
            ToAgent::Delete(DeleterReq::Commit),
            ToAgent::AbortDelete,
            ToAgent::Check(vec![CheckReq::Guards {
                node: NodeRef::from_bits(9),
                rule,
                path: "/srv/web/node_modules".into(),
                vcs: true,
                processes: vec!["node".into(), "".into()],
            }]),
            ToAgent::Measure(vec![measure_req()]),
            ToAgent::EndJob { job: JobId(1) },
            ToAgent::StopDirs {
                job: JobId(1),
                dirs: vec![DirId(3), DirId(u32::MAX)],
            },
            ToAgent::Abandon {
                job: JobId(1),
                dir: DirId(3),
            },
        ];
        for d in downs {
            let bytes = d.encode();
            assert_eq!(ToAgent::decode(&bytes, Some(&set)).unwrap().encode(), bytes);
        }
        // Native commands and sizes go as their rule; the agent builds them from its rules.
        // (Docker's rule, with variants and a size command, is for macOS and Windows.)
        let set = dfdu_rules::builtin(&dfdu_rules::Env::with(
            dfdu_rules::Platform::MacOs,
            &[("HOME", "/Users/u")],
        ))
        .0;
        let docker = set
            .rules()
            .iter()
            .position(|r| {
                r.native
                    .as_ref()
                    .is_some_and(|n| !n.variants.is_empty() && !n.size_command.is_empty())
            })
            .map(|i| RuleIx(i as u16))
            .unwrap();
        let path = std::path::Path::new("/srv/\u{e9}t\u{e9}");
        for (variant, dry_run) in [(None, false), (None, true), (Some(0), false)] {
            let want = crate::applier::native_req(&set, docker, variant, path, dry_run, 5)
                .expect("the rule has the command");
            let d = ToAgent::Delete(DeleterReq::Native(want.clone()));
            let ToAgent::Delete(DeleterReq::Native(got)) =
                ToAgent::decode(&d.encode(), Some(&set)).unwrap()
            else {
                panic!("not a native command");
            };
            assert_eq!(got, want);
            assert!(ToAgent::decode(&d.encode(), None).is_err());
            assert!(ToAgent::decode(&d.encode(), Some(&few_rules())).is_err());
        }
        // A variant the rule does not have, a relative path, or no rule at all are refused.
        let mut bad = crate::applier::native_req(&set, docker, None, path, false, 5).unwrap();
        bad.rule.as_mut().unwrap().variant = Some(99);
        let bytes = ToAgent::Delete(DeleterReq::Native(bad.clone())).encode();
        assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
        bad.rule.as_mut().unwrap().variant = None;
        bad.rule.as_mut().unwrap().path = "srv".into();
        let bytes = ToAgent::Delete(DeleterReq::Native(bad.clone())).encode();
        assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
        bad.rule = None;
        let bytes = ToAgent::Delete(DeleterReq::Native(bad)).encode();
        assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
        let size = ToAgent::Check(vec![CheckReq::NativeSize {
            rule: docker,
            check: vec![],
            command: vec!["anything".into()],
        }]);
        let ToAgent::Check(got) = ToAgent::decode(&size.encode(), Some(&set)).unwrap() else {
            panic!("not a check");
        };
        let n = set.rule(docker).native.as_ref().unwrap();
        assert!(
            matches!(&got[..], [CheckReq::NativeSize { rule, check, command }]
            if *rule == docker && *check == n.check && *command == n.size_command)
        );
        let plain = set.rules().iter().position(|r| r.native.is_none()).unwrap();
        let plain = RuleIx(plain as u16);
        let none = ToAgent::Check(vec![CheckReq::NativeSize {
            rule: plain,
            check: vec![],
            command: vec![],
        }]);
        assert!(ToAgent::decode(&none.encode(), Some(&set)).is_err());
        // A class before the rules, or naming a rule the agent does not have, is refused.
        let job = ToAgent::StartJob {
            job: JobId(1),
            root: b"/".to_vec(),
            cross_mounts: false,
            network: false,
            class: Some(class(rule)),
        };
        assert!(ToAgent::decode(&job.encode(), None).is_err());
        let few = dfdu_rules::load_from(
            &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
            &[],
        )
        .0;
        assert!(ToAgent::decode(&job.encode(), Some(&few)).is_err());
    }

    /// A class is written as the rule's index (a varint), the safety level's number, the
    /// `degrade` condition and the flags; a safety number past the last level is refused.
    #[test]
    fn class_layout() {
        let c = Class {
            rule: RuleIx(300),
            safety: Safety::Review,
            degraded: Some(1),
            flags: Class::CHECKED,
        };
        let mut e = Enc::default();
        enc_class(&mut e, &c);
        assert_eq!(e.0, [0xac, 0x02, 2, 1, 1, 2]);
        let (set, rule) = rules();
        let mut e = Enc::default();
        enc_class(
            &mut e,
            &Class {
                rule,
                degraded: None,
                ..c
            },
        );
        let back = dec_class(&mut Dec::new(&e.0), Some(&set)).unwrap();
        assert_eq!((back.rule, back.safety), (rule, Safety::Review));
        let safety_at = e.0.len() - 3;
        e.0[safety_at] = Safety::ALL.len() as u8;
        assert!(dec_class(&mut Dec::new(&e.0), Some(&set)).is_err());
    }

    /// Frames carry their payload; a flipped bit is a checksum error, a cut frame an error,
    /// and an empty stream the end.
    #[test]
    fn frames_are_checked() {
        let mut out = Vec::new();
        for up in sample() {
            write_frame(&mut out, &up.encode()).unwrap();
        }
        let mut r = &out[..];
        let mut buf = Vec::new();
        let mut n = 0;
        let set = rules().0;
        while read_frame(&mut r, &mut buf).unwrap() {
            FromAgent::decode(&buf, Some(&set)).unwrap();
            n += 1;
        }
        assert_eq!(n, sample().len());
        let mut bad = out.clone();
        bad[20] ^= 1;
        let mut r = &bad[..];
        let errs = (0..n)
            .filter(|_| read_frame(&mut r, &mut buf).is_err())
            .count();
        assert!(errs >= 1);
        assert!(read_frame(&mut &out[..out.len() - 1], &mut buf).is_ok());
        let mut r = &out[..9];
        assert!(read_frame(&mut r, &mut buf).is_err());
        let mut huge = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        huge.extend([0; 4]);
        assert!(matches!(
            read_frame(&mut &huge[..], &mut buf),
            Err(WireError::TooLarge(_))
        ));
    }

    /// No cut or bit flip of a payload panics, and what decodes is self-consistent: spans in
    /// range, marks naming entries, classes naming known rules.
    #[test]
    fn damaged_payloads_never_panic() {
        let set = rules().0;
        for up in sample() {
            let bytes = up.encode();
            for cut in 0..bytes.len() {
                assert!(FromAgent::decode(&bytes[..cut], Some(&set)).is_err());
            }
            for i in 0..bytes.len() * 8 {
                let mut b = bytes.clone();
                b[i / 8] ^= 1 << (i % 8);
                if let Ok(FromAgent::Batch(msgs)) = FromAgent::decode(&b, Some(&set)) {
                    for m in msgs {
                        if let ScanMsg::Chunk(c) = m {
                            for f in &c.files {
                                c.name(f.name);
                            }
                            for s in &c.subdirs {
                                c.name(s.name);
                            }
                        }
                    }
                }
            }
        }
        // Classes of rules the local side does not have are refused, and any without rules.
        let batch = &sample()[1];
        assert!(FromAgent::decode(&batch.encode(), None).is_err());
        let few = dfdu_rules::load_from(
            &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
            &[],
        )
        .0;
        assert!(FromAgent::decode(&batch.encode(), Some(&few)).is_err());
        for d in [
            ToAgent::Rules {
                files: vec![(Some("a".into()), "x".into())],
                root: b"/".to_vec(),
            },
            ToAgent::Delete(DeleterReq::Plan(plan())),
            ToAgent::Measure(vec![measure_req()]),
        ] {
            let bytes = d.encode();
            for cut in 0..bytes.len() {
                assert!(ToAgent::decode(&bytes[..cut], Some(&set)).is_err());
            }
        }
        // A measured name is opened relative to its directory: no paths.
        for name in [&b"a/b"[..], b"..", b".", b""] {
            let mut r = measure_req();
            r.files[1].name = name.to_vec();
            assert!(ToAgent::decode(&ToAgent::Measure(vec![r]).encode(), None).is_err());
        }
        // A shared range cannot end before it starts.
        let mut m = sample().pop().unwrap();
        if let FromAgent::Measured(msgs) = &mut m
            && let MeasureResult::Measured(me) = &mut msgs[0].result
        {
            me.ranges[0] = (9, 8);
        }
        assert!(FromAgent::decode(&m.encode(), None).is_err());
    }
}
