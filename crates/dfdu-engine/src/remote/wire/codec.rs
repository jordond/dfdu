//! The payload encoding: LEB128 varints (signed ones zigzag-encoded), byte strings as a
//! varint length and the bytes.

use dfdu_rules::protected::State;
use dfdu_tree::{Host, Kind, RuleIx, Safety};

use super::WireError;

/// Appends to a payload.
#[derive(Default)]
pub struct Enc(pub Vec<u8>);

impl Enc {
    pub(super) fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub(super) fn var(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push(v.to_le_bytes()[0] | 0x80);
            v >>= 7;
        }
        self.0.push(v.to_le_bytes()[0]);
    }
    pub(super) fn zig(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)).cast_unsigned());
    }
    pub(super) fn bytes(&mut self, b: &[u8]) {
        self.var(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    pub(super) fn bool(&mut self, b: bool) {
        self.u8(u8::from(b));
    }
    pub(super) fn rule(&mut self, r: RuleIx) {
        self.var(r.0.into());
    }
    pub(super) fn safety(&mut self, s: Safety) {
        self.u8(s as u8);
    }
    pub(super) fn kind(&mut self, k: Kind) {
        self.u8(match k {
            Kind::File => 0,
            Kind::Dir => 1,
            Kind::Symlink => 2,
            Kind::Other => 3,
        });
    }
    pub(super) fn protection(&mut self, s: State) {
        self.u8(match s {
            State::Off => 0,
            State::On => 1,
            State::Sticky => 2,
        });
    }
    /// An option: tag 0, or tag 1 and the value.
    pub(super) fn opt<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) {
        match v {
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
            None => self.u8(0),
        }
    }
    /// A list: its length, then each item.
    pub(super) fn list<T>(&mut self, xs: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.var(xs.len() as u64);
        for x in xs {
            f(self, x);
        }
    }
}

/// Reads a payload.
pub struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
    /// The host the paths in the payload are on: the agent's for what the local side reads,
    /// this one for what the agent reads.
    pub(super) host: Host,
}

pub(super) type R<T> = Result<T, WireError>;

pub(super) fn bad<T>(what: &'static str) -> R<T> {
    Err(WireError::Malformed(what))
}

impl<'a> Dec<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Dec::on(buf, Host::LOCAL)
    }

    /// A decoder of a payload whose paths are on `host`.
    #[must_use]
    pub fn on(buf: &'a [u8], host: Host) -> Self {
        Dec { buf, pos: 0, host }
    }
    pub(super) fn u8(&mut self) -> R<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(WireError::Malformed("truncated"))?;
        self.pos += 1;
        Ok(b)
    }
    pub(super) fn var(&mut self) -> R<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return bad("varint overflow");
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bad("varint overflow")
    }
    pub(super) fn u32(&mut self) -> R<u32> {
        u32::try_from(self.var()?).or(bad("u32 out of range"))
    }
    pub(super) fn u16(&mut self) -> R<u16> {
        u16::try_from(self.var()?).or(bad("u16 out of range"))
    }
    pub(super) fn rule(&mut self) -> R<RuleIx> {
        self.u16().map(RuleIx)
    }
    pub(super) fn safety(&mut self) -> R<Safety> {
        Safety::from_u8(self.u8()?).ok_or(WireError::Malformed("safety"))
    }
    pub(super) fn kind(&mut self) -> R<Kind> {
        Ok(match self.u8()? {
            0 => Kind::File,
            1 => Kind::Dir,
            2 => Kind::Symlink,
            3 => Kind::Other,
            _ => return bad("kind"),
        })
    }
    pub(super) fn protection(&mut self) -> R<State> {
        Ok(match self.u8()? {
            0 => State::Off,
            1 => State::On,
            2 => State::Sticky,
            _ => return bad("protection"),
        })
    }
    pub(super) fn zig(&mut self) -> R<i64> {
        let v = self.var()?;
        Ok((v >> 1).cast_signed() ^ -(v & 1).cast_signed())
    }
    pub(super) fn bytes(&mut self) -> R<&'a [u8]> {
        let n = self.var()?;
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| self.pos.checked_add(n))
            .filter(|&e| e <= self.buf.len())
            .ok_or(WireError::Malformed("truncated"))?;
        let b = &self.buf[self.pos..end];
        self.pos = end;
        Ok(b)
    }
    pub(super) fn lossy(&mut self) -> R<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    pub(super) fn u64_le(&mut self) -> R<u64> {
        let end = self.pos.saturating_add(8);
        let b: [u8; 8] = self
            .buf
            .get(self.pos..end)
            .and_then(|b| b.try_into().ok())
            .ok_or(WireError::Malformed("truncated"))?;
        self.pos = end;
        Ok(u64::from_le_bytes(b))
    }
    pub(super) fn text(&mut self) -> R<String> {
        match std::str::from_utf8(self.bytes()?) {
            Ok(t) => Ok(t.to_owned()),
            Err(_) => bad("text not UTF-8"),
        }
    }
    pub(super) fn bool(&mut self) -> R<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => bad("bool"),
        }
    }
    /// A count of items each taking at least `min` bytes, checked against what is left, so a
    /// forged count cannot make a huge allocation.
    pub(super) fn count(&mut self, min: usize) -> R<usize> {
        let n = self.var()?;
        match usize::try_from(n) {
            Ok(n) if n.saturating_mul(min.max(1)) <= self.buf.len() - self.pos => Ok(n),
            _ => bad("count larger than the payload"),
        }
    }
    /// An option written by [`Enc::opt`]; any other tag is a malformed `what`.
    pub(super) fn opt<T>(
        &mut self,
        what: &'static str,
        f: impl FnOnce(&mut Self) -> R<T>,
    ) -> R<Option<T>> {
        match self.u8()? {
            0 => Ok(None),
            1 => f(self).map(Some),
            _ => bad(what),
        }
    }
    /// A list written by [`Enc::list`], of items each taking at least `min` bytes (see
    /// [`Dec::count`]).
    pub(super) fn list<T>(
        &mut self,
        min: usize,
        mut f: impl FnMut(&mut Self) -> R<T>,
    ) -> R<Vec<T>> {
        let n = self.count(min)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }
    pub(super) fn end(&self) -> R<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            bad("trailing bytes")
        }
    }
}
