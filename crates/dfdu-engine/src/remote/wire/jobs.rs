//! The jobs the local side hands the agent on the wire: content checks and measurements, their
//! requests and their answers, and the path and result encodings deletions share with them.

use std::path::PathBuf;

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Measurement;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use dfdu_rules::RuleSet;
use dfdu_tree::NodeRef;

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
                .and_then(|r| r.action.native())
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

pub(super) fn enc_path(e: &mut Enc, p: &std::path::Path) {
    e.bytes(&dfdu_tree::name_bytes(p.as_os_str()));
}

pub(super) fn dec_path(d: &mut Dec) -> R<PathBuf> {
    Ok(PathBuf::from(dfdu_tree::name_os(d.bytes()?).into_owned()))
}

pub(super) fn enc_result<T>(e: &mut Enc, r: &Result<T, String>, ok: impl FnOnce(&mut Enc, &T)) {
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

pub(super) fn dec_result<T>(
    d: &mut Dec,
    ok: impl FnOnce(&mut Dec) -> R<T>,
) -> R<Result<T, String>> {
    Ok(match d.u8()? {
        0 => Ok(ok(d)?),
        1 => Err(d.lossy()?),
        _ => return bad("result"),
    })
}
