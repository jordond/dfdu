//! Encoding and decoding of whole [`FromAgent`] and [`ToAgent`] messages.

use dfdu_rules::RuleSet;

use dfdu_scan::{DirId, JobId};

use super::codec::{Dec, Enc, R, bad};
use super::delete::{dec_del_msg, dec_del_req, enc_del_msg, enc_del_req};
use super::jobs::{
    dec_check_msg, dec_check_req, dec_measure_msg, dec_measure_req, enc_check_msg, enc_check_req,
    enc_measure_msg, enc_measure_req,
};
use super::scan::{dec_msg, dec_wclass, enc_msg, enc_wclass};
use super::{FromAgent, ToAgent};

impl FromAgent {
    #[must_use]
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
                links,
                fingerprint,
                root,
            } => {
                e.u8(6);
                e.bytes(platform.as_bytes());
                for list in [vars, links] {
                    e.var(list.len() as u64);
                    for (k, v) in list {
                        e.bytes(k.as_bytes());
                        e.bytes(v.as_bytes());
                    }
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
                host,
                root,
            } => {
                e.u8(9);
                e.var(*measure_min);
                e.bool(*privileged);
                e.u8(host.to_u8());
                e.bytes(root);
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

    /// Decodes a payload whose paths are on `host` (the agent's); classes must name one of the
    /// local side's `rules`, and there are none without them. [`FromAgent::Rules`]'s root is
    /// not checked: the local side checks it against the set it then builds.
    ///
    /// # Errors
    ///
    /// [`WireError::Malformed`](super::WireError::Malformed) if the payload is not a valid
    /// message, or names a rule the local side does not have.
    pub fn decode(buf: &[u8], rules: Option<&RuleSet>, host: dfdu_tree::Host) -> R<FromAgent> {
        let mut d = Dec::on(buf, host);
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
                host: match dfdu_tree::Host::from_u8(d.u8()?) {
                    Some(h) => h,
                    None => return bad("host"),
                },
                root: d.bytes()?.to_vec(),
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
                let mut pairs = || -> R<Vec<(String, String)>> {
                    let n = d.count(2)?;
                    let mut out = Vec::with_capacity(n);
                    for _ in 0..n {
                        out.push((d.text()?, d.text()?));
                    }
                    Ok(out)
                };
                let (vars, links) = (pairs()?, pairs()?);
                FromAgent::Rules {
                    platform,
                    vars,
                    links,
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
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            ToAgent::Hello { version, root } => {
                e.u8(0);
                e.var((*version).into());
                e.bytes(root);
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
                enter,
                class,
            } => {
                e.u8(1);
                e.var(job.0.into());
                e.bytes(root);
                e.bool(*cross_mounts);
                e.bool(*network);
                e.var(enter.len() as u64);
                for &(dev, ino) in enter {
                    e.var(dev);
                    e.var(ino);
                }
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
    ///
    /// # Errors
    ///
    /// [`WireError::Malformed`](super::WireError::Malformed) if the payload is not a valid
    /// message, or names a rule the agent does not have.
    pub fn decode(buf: &[u8], rules: Option<&RuleSet>) -> R<ToAgent> {
        let mut d = Dec::new(buf);
        let down = match d.u8()? {
            0 => ToAgent::Hello {
                version: d.u32()?,
                root: d.bytes()?.to_vec(),
            },
            1 => ToAgent::StartJob {
                job: JobId(d.u32()?),
                root: d.bytes()?.to_vec(),
                cross_mounts: d.bool()?,
                network: d.bool()?,
                enter: {
                    let n = d.count(2)?;
                    let mut out = Vec::with_capacity(n);
                    for _ in 0..n {
                        out.push((d.var()?, d.var()?));
                    }
                    out
                },
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
