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

/// The first byte of each [`FromAgent`] message.
mod up {
    pub const HELLO: u8 = 0;
    pub const BATCH: u8 = 1;
    pub const HEARTBEAT: u8 = 2;
    pub const STARTED: u8 = 3;
    pub const ABANDONED: u8 = 4;
    pub const FATAL: u8 = 5;
    pub const RULES: u8 = 6;
    pub const DELETER: u8 = 7;
    pub const CHECKED: u8 = 8;
    pub const HOST: u8 = 9;
    pub const MEASURED: u8 = 10;
}

/// The first byte of each [`ToAgent`] message.
mod down {
    pub const HELLO: u8 = 0;
    pub const START_JOB: u8 = 1;
    pub const END_JOB: u8 = 2;
    pub const STOP_DIRS: u8 = 3;
    pub const ABANDON: u8 = 4;
    pub const RULES: u8 = 5;
    pub const DELETE: u8 = 6;
    pub const ABORT_DELETE: u8 = 7;
    pub const CHECK: u8 = 8;
    pub const MEASURE: u8 = 9;
}

impl FromAgent {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        match self {
            FromAgent::Hello { min, max, tool } => {
                e.u8(up::HELLO);
                e.var((*min).into());
                e.var((*max).into());
                e.bytes(tool.as_bytes());
            }
            FromAgent::Batch(msgs) => {
                e.u8(up::BATCH);
                e.list(msgs, enc_msg);
            }
            FromAgent::Heartbeat => e.u8(up::HEARTBEAT),
            FromAgent::Rules {
                platform,
                vars,
                links,
                fingerprint,
                root,
            } => {
                e.u8(up::RULES);
                e.bytes(platform.as_bytes());
                for list in [vars, links] {
                    e.list(list, |e, (k, v)| {
                        e.bytes(k.as_bytes());
                        e.bytes(v.as_bytes());
                    });
                }
                e.var(*fingerprint);
                enc_wclass(&mut e, root);
            }
            FromAgent::Started { job, seq } => {
                e.u8(up::STARTED);
                e.var(job.0.into());
                e.var(*seq);
            }
            FromAgent::Abandoned { job, dir, complete } => {
                e.u8(up::ABANDONED);
                e.var(job.0.into());
                e.var(dir.0.into());
                e.bool(*complete);
            }
            FromAgent::Fatal(s) => {
                e.u8(up::FATAL);
                e.bytes(s.as_bytes());
            }
            FromAgent::Deleter(m) => {
                e.u8(up::DELETER);
                enc_del_msg(&mut e, m);
            }
            FromAgent::Checked(msgs) => {
                e.u8(up::CHECKED);
                e.list(msgs, enc_check_msg);
            }
            FromAgent::Host {
                measure_min,
                privileged,
                host,
                root,
            } => {
                e.u8(up::HOST);
                e.var(*measure_min);
                e.bool(*privileged);
                e.u8(host.to_u8());
                e.bytes(root);
            }
            FromAgent::Measured(msgs) => {
                e.u8(up::MEASURED);
                e.list(msgs, enc_measure_msg);
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
            up::HELLO => FromAgent::Hello {
                min: d.u32()?,
                max: d.u32()?,
                tool: String::from_utf8_lossy(d.bytes()?).into_owned(),
            },
            up::BATCH => FromAgent::Batch(d.list(2, |d| dec_msg(d, rules))?),
            up::HEARTBEAT => FromAgent::Heartbeat,
            up::STARTED => FromAgent::Started {
                job: JobId(d.u32()?),
                seq: d.var()?,
            },
            up::ABANDONED => FromAgent::Abandoned {
                job: JobId(d.u32()?),
                dir: DirId(d.u32()?),
                complete: d.bool()?,
            },
            up::FATAL => FromAgent::Fatal(String::from_utf8_lossy(d.bytes()?).into_owned()),
            up::DELETER => FromAgent::Deleter(dec_del_msg(&mut d)?),
            up::CHECKED => FromAgent::Checked(d.list(3, dec_check_msg)?),
            up::HOST => FromAgent::Host {
                measure_min: d.var()?,
                privileged: d.bool()?,
                host: match dfdu_tree::Host::from_u8(d.u8()?) {
                    Some(h) => h,
                    None => return bad("host"),
                },
                root: d.bytes()?.to_vec(),
            },
            up::MEASURED => FromAgent::Measured(d.list(4, dec_measure_msg)?),
            up::RULES => {
                let platform = d.text()?;
                let mut pairs = || d.list(2, |d| Ok((d.text()?, d.text()?)));
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
                e.u8(down::HELLO);
                e.var((*version).into());
                e.bytes(root);
            }
            ToAgent::Rules { files, root } => {
                e.u8(down::RULES);
                e.list(files, |e, (name, text)| {
                    e.opt(name.as_ref(), |e, n| e.bytes(n.as_bytes()));
                    e.bytes(text.as_bytes());
                });
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
                e.u8(down::START_JOB);
                e.var(job.0.into());
                e.bytes(root);
                e.bool(*cross_mounts);
                e.bool(*network);
                e.list(enter, |e, &(dev, ino)| {
                    e.var(dev);
                    e.var(ino);
                });
                e.opt(class.as_ref(), enc_wclass);
            }
            ToAgent::EndJob { job } => {
                e.u8(down::END_JOB);
                e.var(job.0.into());
            }
            ToAgent::StopDirs { job, dirs } => {
                e.u8(down::STOP_DIRS);
                e.var(job.0.into());
                e.list(dirs, |e, d| e.var(d.0.into()));
            }
            ToAgent::Abandon { job, dir } => {
                e.u8(down::ABANDON);
                e.var(job.0.into());
                e.var(dir.0.into());
            }
            ToAgent::Delete(r) => {
                e.u8(down::DELETE);
                enc_del_req(&mut e, r);
            }
            ToAgent::AbortDelete => e.u8(down::ABORT_DELETE),
            ToAgent::Check(reqs) => {
                e.u8(down::CHECK);
                e.list(reqs, enc_check_req);
            }
            ToAgent::Measure(reqs) => {
                e.u8(down::MEASURE);
                e.list(reqs, enc_measure_req);
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
            down::HELLO => ToAgent::Hello {
                version: d.u32()?,
                root: d.bytes()?.to_vec(),
            },
            down::START_JOB => ToAgent::StartJob {
                job: JobId(d.u32()?),
                root: d.bytes()?.to_vec(),
                cross_mounts: d.bool()?,
                network: d.bool()?,
                enter: d.list(2, |d| Ok((d.var()?, d.var()?)))?,
                class: d.opt("class", |d| match rules {
                    Some(_) => dec_wclass(d, rules),
                    None => bad("a class without rules"),
                })?,
            },
            down::END_JOB => ToAgent::EndJob {
                job: JobId(d.u32()?),
            },
            down::STOP_DIRS => ToAgent::StopDirs {
                job: JobId(d.u32()?),
                dirs: d.list(1, |d| d.u32().map(DirId))?,
            },
            down::ABANDON => ToAgent::Abandon {
                job: JobId(d.u32()?),
                dir: DirId(d.u32()?),
            },
            down::DELETE => ToAgent::Delete(dec_del_req(&mut d, rules)?),
            down::ABORT_DELETE => ToAgent::AbortDelete,
            down::CHECK => ToAgent::Check(d.list(2, |d| dec_check_req(d, rules))?),
            down::MEASURE => ToAgent::Measure(d.list(3, dec_measure_req)?),
            down::RULES => ToAgent::Rules {
                files: d.list(2, |d| Ok((d.opt("rule file name", Dec::text)?, d.text()?)))?,
                root: d.bytes()?.to_vec(),
            },
            _ => return bad("message tag"),
        };
        d.end()?;
        Ok(down)
    }
}
