//! Deletions on the wire: plans and native commands the local side sends, and the agent's
//! results.

use std::sync::Arc;

use dfdu_clean::inside::{Listing, Listings};
use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal};
use dfdu_rules::RuleSet;
use dfdu_tree::NodeRef;

use super::codec::{Dec, Enc, R, bad};
use super::jobs::{dec_path, dec_result, enc_path, enc_result};
use super::scan::{dec_err, enc_err};

#[cfg(test)]
mod tests;

pub(super) fn enc_del_req(e: &mut Enc, r: &DeleterReq) {
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
            e.list(&p.items, |e, it| {
                e.var(it.node.to_bits());
                enc_path(e, &it.path);
                let id = &it.identity;
                e.var(id.dev);
                e.var(id.ino);
                e.kind(id.kind);
                e.var(id.apparent);
                e.zig(id.mtime);
                e.var(it.bytes);
                // Sorted, so a plan always encodes alike.
                let mut dirs: Vec<_> = it.inside.iter().collect();
                dirs.sort_by(|a, b| a.0.cmp(b.0));
                e.list(&dirs, |e, (rel, l)| {
                    enc_path(e, rel);
                    e.var(l.ino);
                    e.list(&l.entries, |e, &(h, ino, k)| {
                        e.0.extend_from_slice(&h.to_le_bytes());
                        e.var(ino);
                        e.kind(k);
                    });
                });
            });
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
                e.opt(r.variant, |e, v| e.var(v.into()));
                enc_path(e, &r.path);
                e.bool(n.dry_run);
            }
            None => e.u8(4),
        },
    }
}

pub(super) fn dec_del_req(d: &mut Dec, rules: Option<&RuleSet>) -> R<DeleterReq> {
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
            let items = d.list(8, |d| {
                let node = NodeRef::from_bits(d.var()?);
                let path = dec_path(d)?;
                let identity = Identity {
                    dev: d.var()?,
                    ino: d.var()?,
                    kind: d.kind()?,
                    apparent: d.var()?,
                    mtime: d.zig()?,
                };
                let bytes = d.var()?;
                let dirs = d.count(3)?;
                let mut inside = Listings::with_capacity(dirs);
                for _ in 0..dirs {
                    let rel = dec_path(d)?;
                    let ino = d.var()?;
                    let entries = d.list(10, |d| Ok((d.u64_le()?, d.var()?, d.kind()?)))?;
                    if inside.insert(rel, Listing { ino, entries }).is_some() {
                        return bad("a directory listed twice");
                    }
                }
                Ok(PlanItem {
                    node,
                    path,
                    identity,
                    bytes,
                    inside: Arc::new(inside),
                })
            })?;
            DeleterReq::Plan(Plan {
                id,
                mode,
                scan_root,
                items,
                protected: None,
            })
        }
        1 => DeleterReq::Undo { plan: d.var()? },
        2 => DeleterReq::Commit,
        3 => {
            let id = d.var()?;
            let rule = d.rule()?;
            let variant = d.opt("native variant", Dec::u16)?;
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
        // 5 was a read-only refusal, which no deleter ever sent.
        6 => Refusal::TrashCrossesFilesystems,
        7 => Refusal::TrashOtherSubvolume(dec_path(d)?),
        8 => Refusal::NoJournal,
        _ => return bad("refusal"),
    })
}

pub(super) fn enc_del_msg(e: &mut Enc, m: &DeleterMsg) {
    let texts = |e: &mut Enc, v: &[String]| e.list(v, |e, s| e.bytes(s.as_bytes()));
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
                    enc_err(e, *err);
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
            enc_result(e, result, enc_path);
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

pub(super) fn dec_del_msg(d: &mut Dec) -> R<DeleterMsg> {
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
            items: d.list(1, Dec::lossy)?,
            notes: d.list(1, Dec::lossy)?,
        },
        _ => return bad("deleter message"),
    })
}
