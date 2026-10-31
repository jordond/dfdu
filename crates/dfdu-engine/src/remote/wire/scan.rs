//! Scanner messages on the wire: metadata, errors, classes, and the messages; their chunks
//! are in `chunk`.

use dfdu_rules::{Hit, Inside, RuleSet};
use dfdu_tree::{Class, ErrKind, Meta};

use dfdu_scan::{DirId, JobId, ListingStatus, ScanMsg};

use super::codec::{Dec, Enc, R, bad};
use super::{WireClass, known};
use chunk::{dec_chunk, enc_chunk};

mod chunk;

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
    e.opt(c.degraded, |e, r| e.var(r.into()));
    e.u8(c.flags);
}

/// A class naming one of `rules` rules; none are known without rules.
pub(super) fn dec_class(d: &mut Dec, rules: Option<&RuleSet>) -> R<Class> {
    let rule = d.rule()?;
    let safety = d.safety()?;
    let degraded = d.opt("degraded", Dec::u16)?;
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
    e.list(&c.inside, |e, &r| e.rule(r));
    e.protection(c.protection);
    e.list(&c.chain, |e, h| {
        e.rule(h.rule);
        e.safety(h.safety);
        e.opt(h.degraded, |e, r| e.var(r.into()));
        e.bool(h.inside == Inside::Check);
        e.bool(h.open);
    });
}

/// A class to classify against; with `rules`, every rule it names must be one of them.
pub(super) fn dec_wclass(d: &mut Dec, rules: Option<&RuleSet>) -> R<WireClass> {
    let path = d.bytes()?.to_vec();
    let inside = d.list(1, Dec::rule)?;
    let protection = d.protection()?;
    let chain = d.list(5, |d| {
        Ok(Hit {
            rule: d.rule()?,
            safety: d.safety()?,
            degraded: d.opt("degraded", Dec::u16)?,
            inside: if d.bool()? {
                Inside::Check
            } else {
                Inside::Done
            },
            open: d.bool()?,
        })
    })?;
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
                    e.opt(*read_error, enc_err);
                }
                ListingStatus::Failed(k) => {
                    e.u8(2);
                    enc_err(e, *k);
                }
                ListingStatus::Cancelled => e.u8(3),
            }
            e.list(classes, |e, &(d, c)| {
                e.var(d.0.into());
                enc_class(e, c);
            });
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
        // An agent runs no resume, but the message has its form like any other.
        ScanMsg::Verified {
            job,
            batch,
            changed,
            gone,
            complete,
        } => {
            e.u8(4);
            e.var(job.0.into());
            e.var((*batch).into());
            for list in [changed, gone] {
                e.list(list, |e, &i| e.var(i.into()));
            }
            e.u8(u8::from(*complete));
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
                    read_error: d.opt("read error", dec_err)?,
                },
                2 => ListingStatus::Failed(dec_err(d)?),
                3 => ListingStatus::Cancelled,
                _ => return bad("listing status"),
            };
            let classes = d.list(5, |d| Ok((DirId(d.u32()?), dec_class(d, rules)?)))?;
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
        4 => {
            let (job, batch) = (JobId(d.u32()?), d.u32()?);
            let changed = d.list(1, Dec::u32)?;
            let gone = d.list(1, Dec::u32)?;
            ScanMsg::Verified {
                job,
                batch,
                changed,
                gone,
                complete: match d.u8()? {
                    0 => false,
                    1 => true,
                    _ => return bad("verified flag"),
                },
            }
        }
        _ => return bad("scanner message"),
    })
}
