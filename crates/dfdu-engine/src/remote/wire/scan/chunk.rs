//! Chunks on the wire: the entries of a listing with their names and marks, and the probe of
//! a job's root.

use dfdu_rules::RuleSet;
use dfdu_tree::Subvol;

use dfdu_scan::{
    Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, Mark, NameSpan, Probe,
    SkipReason, SubdirRec,
};

use super::super::WireError;
use super::super::codec::{Dec, Enc, R, bad};
use super::{dec_class, dec_err, dec_meta, enc_class, enc_err, enc_meta};

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

pub(super) fn enc_chunk(e: &mut Enc, c: &Chunk) {
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

pub(super) fn dec_chunk(d: &mut Dec, rules: Option<&RuleSet>) -> R<Chunk> {
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
