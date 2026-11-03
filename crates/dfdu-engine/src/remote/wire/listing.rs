//! What the agent's host has outside the scan, for a prompt that goes to a folder: the
//! entries of one directory ([`ToAgent::List`](super::ToAgent::List)) and the mounted
//! filesystems ([`ToAgent::Mounts`](super::ToAgent::Mounts)). Both answers are bounded:
//! at most [`MAX_ENTRIES`] entries and [`MAX_MOUNTS`] mounts, each name at most
//! [`MAX_NAME`] bytes and each path at most [`MAX_PATH`]; decoding refuses more.

use super::codec::{Dec, Enc, R, bad};

/// The most entries one listing carries.
pub const MAX_ENTRIES: usize = 10_000;
/// The most mounts one answer carries.
pub const MAX_MOUNTS: usize = 4096;
/// The longest name of an entry, in bytes: 255 UTF-16 units of a Windows name take up to 765.
pub const MAX_NAME: usize = 1024;
/// The longest path asked for or answered, in bytes.
pub const MAX_PATH: usize = 32 << 10;
/// The longest filesystem type.
pub const MAX_FSTYPE: usize = 255;
/// The longest error text.
pub const MAX_ERROR: usize = 4096;

/// An entry of a directory listed on the agent's host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    /// Its name: not empty, not `.` or `..`, and without a separator of the host.
    pub name: Vec<u8>,
    /// It is a directory, or a link to one.
    pub dir: bool,
}

/// A filesystem mounted on the agent's host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMount {
    /// Where it is mounted, absolute.
    pub point: Vec<u8>,
    /// Its type (`ext4`, `zfs`, `nfs4`, `apfs`).
    pub fstype: String,
    /// Bytes in use, when it answered in time.
    pub used: Option<u64>,
    /// Its size, when it answered in time.
    pub total: Option<u64>,
}

/// `text` cut to at most [`MAX_ERROR`] bytes, on a character boundary.
#[must_use]
pub fn clip_error(mut text: String) -> String {
    if text.len() > MAX_ERROR {
        let mut end = MAX_ERROR;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// A result: tag 0 and the value, or tag 1 and the error's text.
pub(super) fn enc_result<T>(e: &mut Enc, r: &Result<T, String>, f: impl FnOnce(&mut Enc, &T)) {
    match r {
        Ok(v) => {
            e.u8(0);
            f(e, v);
        }
        Err(why) => {
            e.u8(1);
            e.bytes(why.as_bytes());
        }
    }
}

pub(super) fn dec_result<'a, T>(
    d: &mut Dec<'a>,
    f: impl FnOnce(&mut Dec<'a>) -> R<T>,
) -> R<Result<T, String>> {
    match d.u8()? {
        0 => f(d).map(Ok),
        1 => Ok(Err(String::from_utf8_lossy(
            d.capped(MAX_ERROR, "error text")?,
        )
        .into_owned())),
        _ => bad("result"),
    }
}

pub(super) fn enc_entries(e: &mut Enc, entries: &[ListEntry]) {
    e.list(entries, |e, x| {
        e.bytes(&x.name);
        e.bool(x.dir);
    });
}

pub(super) fn dec_entries(d: &mut Dec) -> R<Vec<ListEntry>> {
    let host = d.host;
    d.list_upto(MAX_ENTRIES, 3, |d| {
        let name = d.capped(MAX_NAME, "name")?;
        if name.is_empty()
            || name == b"."
            || name == b".."
            || name.iter().any(|&b| host.is_sep(b) || b == 0)
        {
            return bad("entry name");
        }
        Ok(ListEntry {
            name: name.to_vec(),
            dir: d.bool()?,
        })
    })
}

pub(super) fn enc_mounts(e: &mut Enc, mounts: &[HostMount]) {
    e.list(mounts, |e, m| {
        e.bytes(&m.point);
        e.bytes(m.fstype.as_bytes());
        e.opt(m.used, Enc::var);
        e.opt(m.total, Enc::var);
    });
}

pub(super) fn dec_mounts(d: &mut Dec) -> R<Vec<HostMount>> {
    d.list_upto(MAX_MOUNTS, 4, |d| {
        let point = d.capped(MAX_PATH, "mount point")?.to_vec();
        if point.is_empty() {
            return bad("mount point");
        }
        Ok(HostMount {
            point,
            fstype: String::from_utf8_lossy(d.capped(MAX_FSTYPE, "filesystem type")?).into_owned(),
            used: d.opt("used", Dec::var)?,
            total: d.opt("total", Dec::var)?,
        })
    })
}

#[cfg(test)]
mod tests;
