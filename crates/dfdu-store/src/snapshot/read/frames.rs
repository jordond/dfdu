//! The layers under a snapshot's records: the frames, each checked and decompressed as it is
//! read, with the footer checked at the end marker, and the varints and byte strings the
//! records are made of.

use std::io::{self, BufRead, Read};

use super::super::FRAME_MAX;
use crate::ImportError;

/// The little-endian `u32` at byte `i` of `b`.
pub(super) fn le_u32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// The little-endian `u64` at byte `i` of `b`.
fn le_u64(b: &[u8], i: usize) -> u64 {
    u64::from(le_u32(b, i)) | (u64::from(le_u32(b, i + 4)) << 32)
}

/// The frames' raw bytes as one stream, each frame checked as it is read. At the end marker it
/// reads the footer and checks it against what was read.
pub(super) struct FrameReader<R: Read> {
    r: R,
    /// Where in the file the next frame starts.
    at: u64,
    cur: Vec<u8>,
    pos: usize,
    frames: u64,
    crcs: Vec<u8>,
    /// Entries the footer counts, once it has been read.
    pub(super) footer_entries: Option<u64>,
}

/// A damaged record: where it is in the file is not tracked.
pub(super) fn bad(msg: impl Into<String>) -> ImportError {
    ImportError::Format {
        offset: None,
        msg: msg.into(),
    }
}

/// Damage at byte `offset` of the file.
pub(super) fn bad_at(offset: u64, msg: impl Into<String>) -> ImportError {
    ImportError::Format {
        offset: Some(offset),
        msg: msg.into(),
    }
}

impl<R: Read> FrameReader<R> {
    /// The frames of `r`, which starts at byte 12 of the file, past the magic and the version.
    pub(super) fn new(r: R) -> Self {
        FrameReader {
            r,
            at: 12,
            cur: vec![],
            pos: 0,
            frames: 0,
            crcs: vec![],
            footer_entries: None,
        }
    }

    fn next_frame(&mut self) -> Result<bool, ImportError> {
        if self.footer_entries.is_some() {
            return Ok(false);
        }
        let at = self.at;
        let mut h = [0u8; 12];
        self.r.read_exact(&mut h).map_err(truncated)?;
        let (zlen, rawlen, crc) = (
            le_u32(&h, 0) as usize,
            le_u32(&h, 4) as usize,
            le_u32(&h, 8),
        );
        if zlen == 0 {
            if h != [0; 12] {
                return Err(bad_at(at, "a bad end marker"));
            }
            let mut foot = [0u8; 24];
            self.r.read_exact(&mut foot).map_err(truncated)?;
            if crc32c::crc32c(&foot[..20]) != le_u32(&foot, 20) {
                return Err(bad_at(at + 12, "the footer is damaged"));
            }
            if le_u64(&foot, 8) != self.frames || le_u32(&foot, 16) != crc32c::crc32c(&self.crcs) {
                return Err(bad_at(at + 12, "frames are missing or out of order"));
            }
            let mut rest = [0u8; 1];
            if self.r.read(&mut rest)? != 0 {
                return Err(bad_at(at + 36, "data after the footer"));
            }
            self.footer_entries = Some(le_u64(&foot, 0));
            return Ok(false);
        }
        if zlen > FRAME_MAX || rawlen > FRAME_MAX || rawlen == 0 {
            return Err(bad_at(at, "a frame of impossible size"));
        }
        let mut z = vec![0u8; zlen];
        self.r.read_exact(&mut z).map_err(truncated)?;
        if crc32c::crc32c(&z) != crc {
            return Err(bad_at(at, format!("frame {} is damaged", self.frames)));
        }
        let mut d = ruzstd::decoding::StreamingDecoder::new(&z[..])
            .map_err(|e| bad_at(at, format!("frame {}: {e}", self.frames)))?;
        self.cur.clear();
        (&mut d)
            .take(rawlen as u64 + 1)
            .read_to_end(&mut self.cur)
            .map_err(|e| bad_at(at, format!("frame {}: {e}", self.frames)))?;
        if self.cur.len() != rawlen {
            return Err(bad_at(
                at,
                format!("frame {} has the wrong length", self.frames),
            ));
        }
        self.pos = 0;
        self.at += 12 + zlen as u64;
        self.frames += 1;
        self.crcs.extend_from_slice(&crc.to_le_bytes());
        Ok(true)
    }
}

pub(super) fn truncated(e: io::Error) -> ImportError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        bad("the snapshot is cut short")
    } else {
        ImportError::Io(e)
    }
}

impl<R: Read> Read for FrameReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.cur.len() {
            match self.next_frame() {
                Ok(true) => {}
                Ok(false) => return Ok(0),
                Err(ImportError::Io(e)) => return Err(e),
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// An error from reading the records, with the frame reader's own [`ImportError`] taken back
/// out of the `io::Error` it travelled in.
pub(super) fn frame_error(e: io::Error) -> ImportError {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => bad("the records end early"),
        io::ErrorKind::InvalidData => {
            let text = e.to_string();
            match e.into_inner().map(|b| b.downcast::<ImportError>()) {
                Some(Ok(ie)) => *ie,
                _ => bad(text),
            }
        }
        _ => ImportError::Io(e),
    }
}

pub(super) struct Rec<R: BufRead>(pub(super) R);

impl<R: BufRead> Rec<R> {
    pub(super) fn byte(&mut self) -> Result<u8, ImportError> {
        let mut b = [0u8; 1];
        self.0.read_exact(&mut b).map_err(frame_error)?;
        Ok(b[0])
    }

    pub(super) fn u(&mut self) -> Result<u64, ImportError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("a number too long"))
    }

    pub(super) fn i(&mut self) -> Result<i64, ImportError> {
        let u = self.u()?;
        Ok((u >> 1).cast_signed() ^ -(u & 1).cast_signed())
    }

    pub(super) fn bytes(&mut self, max: usize) -> Result<Vec<u8>, ImportError> {
        let n = usize::try_from(self.u()?)
            .ok()
            .filter(|&n| n <= max)
            .ok_or_else(|| ImportError::Limit(format!("a name longer than {max} bytes")))?;
        let mut v = vec![0u8; n];
        for b in &mut v {
            *b = self.byte()?;
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests;
