//! Scanner messages on the wire: metadata, errors, classes, probes, chunks.

use dfdu_rules::protected::State;
use dfdu_rules::{Hit, Inside, RuleSet};
use dfdu_tree::{Class, ErrKind, Meta, Subvol};

use dfdu_scan::{
    Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, ListingStatus, Mark, NameSpan,
    Probe, ScanMsg, SkipReason, SubdirRec,
};

use super::codec::{Dec, Enc, R, bad};
use super::{WireClass, WireError, known};

fn enc_meta(e: &mut Enc, m: &Meta) {
    e.kind(m.kind);
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
        kind: d.kind()?,
        apparent: d.var()?,
        alloc: d.var()?,
        mtime: d.zig()?,
        dev: d.var()?,
        ino: d.var()?,
        nlink: d.u32()?,
        mount: d.var()?,
    })
}

pub(super) fn enc_err(e: &mut Enc, k: ErrKind) {
    match k {
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

pub(super) fn dec_err(d: &mut Dec) -> R<ErrKind> {
    Ok(match d.u8()? {
        0 => ErrKind::PermissionDenied,
        1 => ErrKind::NotFound,
        2 => ErrKind::Os(i32::try_from(d.zig()?).or(bad("errno"))?),
        3 => ErrKind::Stalled,
        4 => ErrKind::Other,
        _ => return bad("error kind"),
    })
}

pub(super) fn enc_class(e: &mut Enc, c: Class) {
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
pub(super) fn dec_class(d: &mut Dec, rules: Option<&RuleSet>) -> R<Class> {
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

pub(super) fn enc_wclass(e: &mut Enc, c: &WireClass) {
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
pub(super) fn dec_wclass(d: &mut Dec, rules: Option<&RuleSet>) -> R<WireClass> {
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
                Some(parent) => {
                    e.u8(1);
                    e.0.extend_from_slice(&parent);
                }
                None => e.u8(0),
            }
            e.bool(s.read_only);
            e.zig(s.otime);
            match s.qgroup {
                Some((rfer, excl)) => {
                    e.u8(1);
                    e.var(rfer);
                    e.var(excl);
                }
                None => e.u8(0),
            }
        }
        None => e.u8(0),
    }
    e.bool(p.shares_extents);
    match p.profile {
        Some((profile, ratio)) => {
            e.u8(1);
            e.u8(profile as u8);
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
            let profile =
                dfdu_sys::Profile::from_u8(d.u8()?).ok_or(WireError::Malformed("profile"))?;
            let mut f = [0u8; 8];
            for b in &mut f {
                *b = d.u8()?;
            }
            let ratio = f64::from_le_bytes(f);
            if !(ratio.is_finite() && ratio > 0.0) {
                return bad("profile ratio");
            }
            Some((profile, ratio))
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
            // resume-keep
            Some(SkipReason::Kept) => 6,
        });
    }
    e.var(c.errors.len() as u64);
    for x in &c.errors {
        enc_span(e, x.name);
        enc_err(e, x.err);
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
                enc_class(e, *c);
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
                // resume-keep
                6 => Some(SkipReason::Kept),
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

pub(super) fn enc_msg(e: &mut Enc, m: &ScanMsg) {
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
                            enc_err(e, *k);
                        }
                        None => e.u8(0),
                    }
                }
                ListingStatus::Failed(k) => {
                    e.u8(2);
                    enc_err(e, *k);
                }
                ListingStatus::Cancelled => e.u8(3),
            }
            e.var(classes.len() as u64);
            for (d, c) in classes {
                e.var(d.0.into());
                enc_class(e, *c);
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

pub(super) fn dec_msg(d: &mut Dec, rules: Option<&RuleSet>) -> R<ScanMsg> {
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
