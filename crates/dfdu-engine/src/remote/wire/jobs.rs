//! The jobs the local side hands the agent on the wire: content checks and measurements, their
//! requests and their answers, and the path and result encodings deletions share with them.

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Measurement;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use dfdu_rules::RuleSet;
use dfdu_tree::{Host, HostPath, NodeRef};

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
            e.list(processes, |e, p| e.bytes(p.as_bytes()));
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
            let processes = d.list(1, Dec::lossy)?;
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
            e.opt(running.as_ref(), |e, p| e.bytes(p.as_bytes()));
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
            running: d.opt("running", Dec::lossy)?,
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
    e.list(&r.files, |e, f| {
        e.var(f.node.to_bits());
        e.bytes(&f.name);
        e.var(f.inode.0);
        e.var(f.inode.1);
    });
}

pub(super) fn dec_measure_req(d: &mut Dec) -> R<MeasureReq> {
    let round = d.var()?;
    let dir = dec_path(d)?;
    let files = d.list(4, |d| {
        let node = NodeRef::from_bits(d.var()?);
        let name = d.bytes()?;
        // A name, not a path: the measurer opens it relative to `dir`.
        if name.is_empty()
            || name.iter().any(|&b| Host::LOCAL.is_sep(b))
            || name == b"."
            || name == b".."
        {
            return bad("file name");
        }
        Ok(MeasureFile {
            node,
            name: name.to_vec(),
            inode: (d.var()?, d.var()?),
        })
    })?;
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
            for list in [&me.ranges, &me.points] {
                e.list(list, |e, &(a, b)| {
                    e.var(a);
                    e.var(b);
                });
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
            let ranges = d.list(2, |d| {
                let (a, b) = (d.var()?, d.var()?);
                if a > b {
                    return bad("extent range");
                }
                Ok((a, b))
            })?;
            let points = d.list(2, |d| Ok((d.var()?, d.var()?)))?;
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

pub(super) fn enc_path(e: &mut Enc, p: &HostPath) {
    e.bytes(p.as_bytes());
}

/// A path on the host the payload's paths are on ([`Dec::on`]).
pub(super) fn dec_path(d: &mut Dec) -> R<HostPath> {
    let host = d.host;
    Ok(HostPath::new(host, d.bytes()?.to_vec()))
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
