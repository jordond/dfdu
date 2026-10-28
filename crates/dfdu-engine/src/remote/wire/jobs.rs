//! The jobs the local side hands the agent on the wire: content checks, measurements and
//! deletions, their requests and their answers.

use std::path::PathBuf;
use std::sync::Arc;

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Measurement;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use dfdu_clean::inside::{Listing, Listings};
use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal};
use dfdu_rules::RuleSet;
use dfdu_tree::{Kind, NodeRef};

use super::codec::{Dec, Enc, R, bad};
use super::scan::{dec_err, enc_err};

pub(super) fn enc_check_req(e: &mut Enc, r: &CheckReq) {
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

pub(super) fn dec_check_req(d: &mut Dec, rules: Option<&RuleSet>) -> R<CheckReq> {
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

pub(super) fn enc_check_msg(e: &mut Enc, m: &CheckMsg) {
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

#[expect(
    clippy::redundant_closure_for_method_calls,
    reason = "`Dec::var` as a path is not general enough over the decoder's lifetimes"
)]
pub(super) fn dec_check_msg(d: &mut Dec) -> R<CheckMsg> {
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

pub(super) fn enc_measure_req(e: &mut Enc, r: &MeasureReq) {
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

pub(super) fn dec_measure_req(d: &mut Dec) -> R<MeasureReq> {
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

pub(super) fn enc_measure_msg(e: &mut Enc, m: &MeasureMsg) {
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
            enc_err(e, *k);
        }
    }
}

pub(super) fn dec_measure_msg(d: &mut Dec) -> R<MeasureMsg> {
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

pub(super) fn enc_del_msg(e: &mut Enc, m: &DeleterMsg) {
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

pub(super) fn dec_del_msg(d: &mut Dec) -> R<DeleterMsg> {
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
