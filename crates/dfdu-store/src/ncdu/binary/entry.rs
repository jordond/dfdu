//! One entry: its CBOR map, read into the info block the JSON import would have read.

use super::bad;
use crate::ImportError;
use crate::Limits;
use crate::ncdu::Info;

pub(super) enum Ref {
    Abs(u64),
    /// Bytes back from the start of the entry that refers.
    Rel(u64),
}

pub(super) struct RawItem {
    pub(super) typ: i64,
    pub(super) info: Info,
    pub(super) rderr: bool,
    pub(super) prev: Option<Ref>,
    pub(super) sub: Option<Ref>,
}

pub(super) struct Item {
    pub(super) typ: i64,
    pub(super) info: Info,
    pub(super) rderr: bool,
    pub(super) prev: Option<u64>,
    pub(super) sub: Option<u64>,
}

impl Item {
    /// The info block the JSON import would have read for this entry.
    pub(super) fn into_info(self) -> (Info, bool) {
        let mut info = self.info;
        let listed = match self.typ {
            0 => {
                info.read_error = self.rderr;
                true
            }
            1 => false,
            3 => {
                info.hlnkc = true;
                false
            }
            -1 => {
                info.read_error = true;
                false
            }
            -3 => {
                info.excluded = Some(b"otherfs".to_vec());
                false
            }
            -4 => {
                info.excluded = Some(b"kernfs".to_vec());
                false
            }
            // -2 and unknown negative types: excluded by a pattern.
            t if t < 0 => {
                info.excluded = Some(b"pattern".to_vec());
                false
            }
            // 2 and unknown positive types: not a regular file.
            _ => {
                info.notreg = true;
                false
            }
        };
        if listed {
            // A directory, whatever its mode says.
            info.mode = Some(0o040_000);
        } else if info.mode.is_some_and(|m| m & 0o170_000 == 0o040_000) {
            info.mode = None;
        }
        (info, listed)
    }
}

/// A reader of one CBOR map, within one decompressed block.
pub(super) struct Cbor<'a> {
    pub(super) b: &'a [u8],
    pub(super) pos: usize,
    pub(super) block: u64,
}

/// Nesting of skipped values (arrays, maps, tags) accepted.
const MAX_NEST: u32 = 16;

impl Cbor<'_> {
    fn err(&self, msg: impl Into<String>) -> ImportError {
        bad(
            self.pos as u64,
            format!("block {}: {}", self.block, msg.into()),
        )
    }

    fn byte(&mut self) -> Result<u8, ImportError> {
        let v = *self
            .b
            .get(self.pos)
            .ok_or_else(|| self.err("entry cut short"))?;
        self.pos += 1;
        Ok(v)
    }

    /// A head: major type and argument; `None` for an indefinite length (or a break, with
    /// major type 7).
    fn head(&mut self) -> Result<(u8, Option<u64>), ImportError> {
        let h = self.byte()?;
        let (major, info) = (h >> 5, h & 0x1f);
        let n = match info {
            0..=23 => Some(u64::from(info)),
            24..=27 => {
                let len = 1usize << (info - 24);
                let s = self
                    .b
                    .get(self.pos..self.pos + len)
                    .ok_or_else(|| self.err("entry cut short"))?;
                self.pos += len;
                Some(s.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
            }
            31 => None,
            _ => return Err(self.err(format!("bad CBOR head {h:#04x}"))),
        };
        Ok((major, n))
    }

    fn int(&mut self) -> Result<i128, ImportError> {
        match self.head()? {
            (0, Some(n)) => Ok(i128::from(n)),
            (1, Some(n)) => Ok(-1 - i128::from(n)),
            _ => Err(self.err("expected an integer")),
        }
    }

    fn uint(&mut self) -> Result<u64, ImportError> {
        u64::try_from(self.int()?).map_err(|_| self.err("expected an unsigned integer"))
    }

    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, ImportError> {
        match self.head()? {
            (2 | 3, Some(n)) => {
                let n = usize::try_from(n).unwrap_or(usize::MAX);
                if n > max {
                    return Err(self.err(format!("a name of {n} bytes")));
                }
                let s = self
                    .b
                    .get(self.pos..self.pos + n)
                    .ok_or_else(|| self.err("entry cut short"))?;
                self.pos += n;
                Ok(s.to_vec())
            }
            (2 | 3, None) => Err(self.err("indefinite-length strings are not supported")),
            _ => Err(self.err("expected a string")),
        }
    }

    fn boolean(&mut self) -> Result<bool, ImportError> {
        match self.head()? {
            (7, Some(20)) => Ok(false),
            (7, Some(21)) => Ok(true),
            _ => Err(self.err("expected true or false")),
        }
    }

    fn reference(&mut self) -> Result<Ref, ImportError> {
        match self.head()? {
            (0, Some(n)) => Ok(Ref::Abs(n)),
            (1, Some(n)) => Ok(Ref::Rel(n.saturating_add(1))),
            _ => Err(self.err("expected a reference")),
        }
    }

    /// Whether a break ends an indefinite map or array here (and reads it).
    fn at_break(&mut self) -> bool {
        if self.b.get(self.pos) == Some(&0xff) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip(&mut self, depth: u32) -> Result<(), ImportError> {
        if depth > MAX_NEST {
            return Err(self.err("values nested too deep"));
        }
        let (major, n) = self.head()?;
        match (major, n) {
            (0 | 1 | 7, Some(_)) => {}
            (2 | 3, Some(n)) => {
                let n = usize::try_from(n).unwrap_or(usize::MAX);
                if self.b.len() - self.pos < n {
                    return Err(self.err("entry cut short"));
                }
                self.pos += n;
            }
            (4 | 5, Some(n)) => {
                let per = if major == 5 { 2 } else { 1 };
                for _ in 0..n.saturating_mul(per) {
                    self.skip(depth + 1)?;
                }
            }
            (2..=5, None) => {
                while !self.at_break() {
                    self.skip(depth + 1)?;
                }
            }
            (6, Some(_)) => self.skip(depth + 1)?,
            _ => return Err(self.err("bad CBOR value")),
        }
        Ok(())
    }

    pub(super) fn item(&mut self, limits: &Limits) -> Result<RawItem, ImportError> {
        let (5, n) = self.head()? else {
            return Err(self.err("an entry that is not a map"));
        };
        let mut it = RawItem {
            typ: i64::MIN,
            info: Info::default(),
            rderr: false,
            prev: None,
            sub: None,
        };
        let mut named = false;
        let mut i = 0u64;
        loop {
            match n {
                Some(n) if i == n => break,
                None if self.at_break() => break,
                _ => {}
            }
            i += 1;
            let key = self.int()?;
            match key {
                0 => {
                    it.typ =
                        i64::try_from(self.int()?).map_err(|_| self.err("type out of range"))?;
                }
                1 => {
                    it.info.name = self.bytes(limits.max_name)?;
                    named = true;
                }
                2 => it.prev = Some(self.reference()?),
                3 => it.info.asize = self.uint()?,
                4 => it.info.dsize = self.uint()?,
                5 => it.info.dev = Some(self.uint()?),
                6 => it.rderr = self.boolean()?,
                12 => it.sub = Some(self.reference()?),
                13 => it.info.ino = self.uint()?,
                14 => it.info.nlink = self.uint()?,
                17 => it.info.mode = Some(self.uint()?),
                18 => it.info.mtime = self.uint()?,
                _ => self.skip(0)?,
            }
        }
        if it.typ == i64::MIN {
            return Err(self.err("an entry without a type"));
        }
        if !named {
            return Err(self.err("an entry without a name"));
        }
        Ok(it)
    }
}

#[cfg(test)]
mod tests;
