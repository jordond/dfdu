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
    e.opt(p.subvol.as_ref(), |e, s| {
        e.var(s.id);
        e.0.extend_from_slice(&s.uuid);
        e.opt(s.parent_uuid, |e, parent| e.0.extend_from_slice(&parent));
        e.bool(s.read_only);
        e.zig(s.otime);
        e.opt(s.qgroup, |e, (rfer, excl)| {
            e.var(rfer);
            e.var(excl);
        });
    });
    e.bool(p.shares_extents);
    e.opt(p.profile, |e, (profile, ratio)| {
        e.u8(profile as u8);
        e.0.extend_from_slice(&ratio.to_le_bytes());
    });
}

fn dec_probe(d: &mut Dec) -> R<Probe> {
    let uuid = |d: &mut Dec| -> R<[u8; 16]> {
        let mut u = [0u8; 16];
        for b in &mut u {
            *b = d.u8()?;
        }
        Ok(u)
    };
    let subvol = d.opt("subvolume", |d| {
        Ok(Subvol {
            id: d.var()?,
            uuid: uuid(d)?,
            parent_uuid: d.opt("parent uuid", uuid)?,
            read_only: d.bool()?,
            otime: d.zig()?,
            qgroup: d.opt("qgroup", |d| Ok((d.var()?, d.var()?)))?,
        })
    })?;
    let shares_extents = d.bool()?;
    let profile = d.opt("profile", |d| {
        let profile = dfdu_sys::Profile::from_u8(d.u8()?).ok_or(WireError::Malformed("profile"))?;
        let mut f = [0u8; 8];
        for b in &mut f {
            *b = d.u8()?;
        }
        let ratio = f64::from_le_bytes(f);
        if !(ratio.is_finite() && ratio > 0.0) {
            return bad("profile ratio");
        }
        Ok((profile, ratio))
    })?;
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
    e.opt(c.root_meta.as_ref(), enc_meta);
    e.opt(c.probe.as_deref(), enc_probe);
    e.bytes(&c.names);
    e.list(&c.files, |e, f| {
        enc_span(e, f.name);
        enc_meta(e, &f.meta);
    });
    e.var(c.first_subdir.0.into());
    e.list(&c.subdirs, |e, s| {
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
    });
    e.list(&c.errors, |e, x| {
        enc_span(e, x.name);
        enc_err(e, x.err);
    });
    e.list(&c.marks, |e, (entry, mark)| {
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
    });
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
    let root_meta = d.opt("root meta", dec_meta)?;
    let probe = d.opt("probe", |d| dec_probe(d).map(Box::new))?;
    let names = d.bytes()?.to_vec();
    let files = d.list(2 + MIN_META, |d| {
        Ok(FileRec {
            name: dec_span(d, names.len())?,
            meta: dec_meta(d)?,
        })
    })?;
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
    let errors = d.list(3, |d| {
        Ok(EntryError {
            name: dec_span(d, names.len())?,
            err: dec_err(d)?,
        })
    })?;
    let marks = d.list(3, |d| {
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
        Ok((entry, mark))
    })?;
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

#[cfg(test)]
mod tests;
