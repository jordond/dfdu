//! NTFS master file table parsing, for the MFT backend on Windows (`WindowsBackend`
//! with `mft`): the boot sector, file records with their update sequence fixups, the
//! attributes the scan needs and data runs. Pure code on byte buffers, so it is tested (and
//! fuzzed) on every platform; reading the volume is the Windows backend's.
//!
//! [`MftBuilder`] takes records in any order and builds an [`MftIndex`]: every name of every
//! file in use with what a directory enumeration reports for it ([`WinStat`]), grouped by the
//! directory holding it. Attributes in extension records (a file with many names or a
//! fragmented data stream) are merged into their base record. Names only in the DOS namespace
//! (the short 8.3 aliases) are left out, as directory listings leave them out; each remaining
//! name is a hard link.
//!
//! Every offset and length read from a record is checked against the record, so a damaged
//! record is skipped, never a panic.

use std::collections::HashMap;

use dfdu_tree::{WinStat, win_attr};

/// The geometry the boot sector states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boot {
    pub sector: u32,
    pub cluster: u32,
    /// Cluster number of the start of the `$MFT`.
    pub mft_lcn: u64,
    /// Bytes per file record (1024 on nearly every volume).
    pub record: u32,
}

impl Boot {
    #[must_use]
    pub fn parse(b: &[u8]) -> Option<Boot> {
        if b.get(3..11)? != b"NTFS    " {
            return None;
        }
        let sector = u32::from(u16_at(b, 0x0B)?);
        let spc = *b.get(0x0D)?;
        // Above 0x80 the count is a negative power of two: 2^(256 - n) sectors.
        let spc = if spc > 0x80 {
            1u32.checked_shl(256 - u32::from(spc))?
        } else {
            u32::from(spc)
        };
        let cluster = sector.checked_mul(spc)?;
        let cpr = b.get(0x40)?.cast_signed();
        let record = if cpr < 0 {
            1u32.checked_shl(u32::from(cpr.unsigned_abs()))?
        } else {
            cluster.checked_mul(u32::from(cpr.cast_unsigned()))?
        };
        let pow2 = |n: u32| n.is_power_of_two();
        if !(512..=4096).contains(&sector)
            || !pow2(sector)
            || !pow2(cluster)
            || cluster > 2 << 20
            || !(512..=65536).contains(&record)
            || !pow2(record)
        {
            return None;
        }
        Some(Boot {
            sector,
            cluster,
            mft_lcn: u64_at(b, 0x30)?,
            record,
        })
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The update sequence stride: the last two bytes of every 512 bytes of a record are saved in
/// its update sequence array and replaced by the sequence number, so a torn write shows.
const STRIDE: usize = 512;

/// Restores a file record's protected bytes; `false` when it is not a valid record (wrong
/// magic, an array out of bounds, or a sector whose check value does not match).
pub fn fixup(rec: &mut [u8]) -> bool {
    if rec.get(..4) != Some(b"FILE") || !rec.len().is_multiple_of(STRIDE) {
        return false;
    }
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (off, count) = (off as usize, count as usize);
    // One check value, then one saved pair per sector.
    if count == 0 || count - 1 != rec.len() / STRIDE || off + 2 * count > rec.len() {
        return false;
    }
    let check = [rec[off], rec[off + 1]];
    for i in 1..count {
        let end = i * STRIDE;
        if rec[end - 2..end] != check {
            return false;
        }
        let saved = off + 2 * i;
        rec[end - 2] = rec[saved];
        rec[end - 1] = rec[saved + 1];
    }
    true
}

/// A run of clusters of a non-resident attribute: where it starts on the volume (`None` for a
/// sparse run) and how many clusters it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub lcn: Option<u64>,
    pub len: u64,
}

/// Decodes a mapping pairs array; `None` when it is malformed (a run past the buffer, a length
/// of zero, a cluster number below zero).
#[must_use]
pub fn data_runs(b: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut at = 0;
    let mut lcn: i64 = 0;
    loop {
        let head = *b.get(at)?;
        if head == 0 {
            return Some(runs);
        }
        let (nlen, noff) = ((head & 0xF) as usize, (head >> 4) as usize);
        if nlen == 0 || nlen > 8 || noff > 8 {
            return None;
        }
        let len = le_unsigned(b.get(at + 1..at + 1 + nlen)?);
        let off = b.get(at + 1 + nlen..at + 1 + nlen + noff)?;
        if len == 0 {
            return None;
        }
        let start = if noff == 0 {
            None
        } else {
            lcn = lcn.checked_add(le_signed(off))?;
            Some(u64::try_from(lcn).ok()?)
        };
        runs.push(Run { lcn: start, len });
        at += 1 + nlen + noff;
    }
}

fn le_unsigned(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |n, &x| n << 8 | u64::from(x))
}

fn le_signed(b: &[u8]) -> i64 {
    let n = le_unsigned(b);
    let bits = 8 * b.len();
    if bits < 64 && n >> (bits - 1) & 1 == 1 {
        (n | !0u64 << bits).cast_signed()
    } else {
        n.cast_signed()
    }
}

/// The runs and length in bytes of a record's unnamed data stream when it is not resident:
/// for record 0, where the `$MFT` itself is. Fixups must have been applied. `None` when the
/// stream is resident, missing or malformed, or continues in an extension record (a `$MFT`
/// in so many fragments is not read).
#[must_use]
pub fn mft_data(rec: &[u8]) -> Option<(Vec<Run>, u64)> {
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let mut at = u16_at(rec, 0x14)? as usize;
    while at + 8 <= used {
        let ty = u32_at(rec, at)?;
        let len = u32_at(rec, at + 4)? as usize;
        if ty == AT_END || len < 16 || at + len > used {
            return None;
        }
        let a = &rec[at..at + len];
        if ty == AT_DATA && *a.get(9)? == 0 {
            if *a.get(8)? == 0 || u64_at(a, 0x10)? != 0 {
                return None;
            }
            let runs = data_runs(a.get(u16_at(a, 0x20)? as usize..)?)?;
            let clusters = runs.iter().try_fold(0u64, |n, r| n.checked_add(r.len))?;
            // The runs must cover the stream: the rest would be in an extension record.
            if clusters <= u64_at(a, 0x18)? {
                return None;
            }
            return Some((runs, u64_at(a, 0x30)?));
        }
        at += len;
    }
    None
}

const AT_STANDARD_INFORMATION: u32 = 0x10;
const AT_FILE_NAME: u32 = 0x30;
const AT_DATA: u32 = 0x80;
const AT_INDEX_ALLOCATION: u32 = 0xA0;
const AT_END: u32 = 0xFFFF_FFFF;

const REC_IN_USE: u16 = 1;
const REC_DIRECTORY: u16 = 2;

/// Attribute flags.
const ATTR_COMPRESSED: u16 = 0x0001;
const ATTR_SPARSE: u16 = 0x8000;

/// Filename namespaces: the DOS one holds the short alias of a name in the Win32 one.
const NS_DOS: u8 = 2;

/// Records below this number are the filesystem's own files (`$MFT`, `$LogFile`, `$Extend`
/// and reserved ones), which directory listings never show.
pub const FIRST_USER_RECORD: u64 = 24;

/// The root directory's record.
pub const ROOT_RECORD: u64 = 5;

/// What one record (or its extensions) contributes to a file.
#[derive(Clone, Debug, Default)]
struct File {
    seq: u16,
    dir: bool,
    in_use: bool,
    /// From `$STANDARD_INFORMATION`.
    attributes: u32,
    last_write: u64,
    /// The unnamed data stream: `(size, allocation)`.
    data: Option<(u64, u64)>,
    /// The directory index allocation (`$INDEX_ALLOCATION:$I30`).
    index_alloc: u64,
    reparse_tag: u32,
    /// `(parent reference, offset and length in the builder's names)`.
    names: Vec<(u64, u32, u16)>,
}

/// Collects records ([`MftBuilder::add`]) into an [`MftIndex`].
#[derive(Default)]
pub struct MftBuilder {
    files: HashMap<u64, File>,
    names: Vec<u16>,
    /// Records that could not be read (a failed fixup, a malformed attribute).
    pub damaged: u64,
}

impl MftBuilder {
    /// Adds record number `n` as read from the volume (its fixups not yet applied). Unused and
    /// damaged records are skipped.
    pub fn add(&mut self, n: u64, rec: &mut [u8]) {
        if !fixup(rec) {
            // Never-used records have no magic: not damage.
            if rec.get(..4) == Some(b"FILE") {
                self.damaged += 1;
            }
            return;
        }
        if self.parse(n, rec).is_none() {
            self.damaged += 1;
        }
    }

    fn parse(&mut self, n: u64, rec: &[u8]) -> Option<()> {
        let flags = u16_at(rec, 0x16)?;
        if flags & REC_IN_USE == 0 {
            return Some(());
        }
        let seq = u16_at(rec, 0x10)?;
        let base = u64_at(rec, 0x20)? & 0xFFFF_FFFF_FFFF;
        let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
        let mut at = u16_at(rec, 0x14)? as usize;
        // An extension record's attributes belong to its base record's file.
        let owner = if base != 0 { base } else { n };
        let mut found = File::default();
        if base == 0 {
            found.seq = seq;
            found.in_use = true;
            found.dir = flags & REC_DIRECTORY != 0;
        }
        while at + 8 <= used {
            let ty = u32_at(rec, at)?;
            if ty == AT_END {
                break;
            }
            let len = u32_at(rec, at + 4)? as usize;
            if len < 16 || at + len > used {
                return None;
            }
            let a = &rec[at..at + len];
            self.attribute(a, ty, &mut found)?;
            at += len;
        }
        let f = self.files.entry(owner).or_default();
        if base == 0 {
            f.seq = found.seq;
            f.in_use = true;
            f.dir = found.dir;
        }
        if found.attributes != 0 || found.last_write != 0 {
            f.attributes = found.attributes;
            f.last_write = found.last_write;
        }
        if found.data.is_some() {
            f.data = found.data;
        }
        f.index_alloc = f.index_alloc.max(found.index_alloc);
        if found.reparse_tag != 0 {
            f.reparse_tag = found.reparse_tag;
        }
        f.names.extend(found.names);
        Some(())
    }

    fn attribute(&mut self, a: &[u8], ty: u32, f: &mut File) -> Option<()> {
        let nonresident = *a.get(8)? != 0;
        let name_len = *a.get(9)? as usize;
        let flags = u16_at(a, 0x0C)?;
        let value = |a: &[u8]| -> Option<(usize, usize)> {
            let len = u32_at(a, 0x10)? as usize;
            let off = u16_at(a, 0x14)? as usize;
            (off.checked_add(len)? <= a.len()).then_some((off, len))
        };
        match ty {
            AT_STANDARD_INFORMATION if !nonresident => {
                let (off, len) = value(a)?;
                let v = &a[off..off + len];
                f.last_write = u64_at(v, 8)?;
                f.attributes = u32_at(v, 32)?;
            }
            AT_FILE_NAME if !nonresident => {
                let (off, len) = value(a)?;
                let v = &a[off..off + len];
                let parent = u64_at(v, 0)?;
                let chars = *v.get(0x40)?;
                let ns = *v.get(0x41)?;
                let name = v.get(0x42..0x42 + 2 * usize::from(chars))?;
                if f.reparse_tag == 0 && u32_at(v, 0x38)? & win_attr::REPARSE_POINT != 0 {
                    f.reparse_tag = u32_at(v, 0x3C)?;
                }
                if ns != NS_DOS && chars > 0 {
                    let start = u32::try_from(self.names.len()).ok()?;
                    self.names.extend(
                        name.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|&c| u16::from_le_bytes(c)),
                    );
                    f.names.push((parent, start, u16::from(chars)));
                }
            }
            AT_DATA if name_len == 0 => {
                if nonresident {
                    // Sizes are in the fragment starting at cluster 0 only.
                    if u64_at(a, 0x10)? == 0 {
                        let alloc = if flags & (ATTR_COMPRESSED | ATTR_SPARSE) != 0
                            && u16_at(a, 0x20)? >= 0x48
                        {
                            u64_at(a, 0x40)?
                        } else {
                            u64_at(a, 0x28)?
                        };
                        f.data = Some((u64_at(a, 0x30)?, alloc));
                    }
                } else {
                    let (_, len) = value(a)?;
                    let len = len as u64;
                    f.data = Some((len, len.div_ceil(8) * 8));
                }
            }
            AT_INDEX_ALLOCATION if nonresident && u64_at(a, 0x10)? == 0 => {
                f.index_alloc = u64_at(a, 0x28)?;
            }
            _ => {}
        }
        Some(())
    }

    /// The index of every file in use, its names grouped by directory.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "entries are names, whose offsets `MftBuilder::add` keeps within `u32`"
    )]
    pub fn finish(self) -> MftIndex {
        let MftBuilder { files, names, .. } = self;
        let mut entries: Vec<MftEntry> = Vec::new();
        for (&n, f) in &files {
            // Extension records whose base never appeared, and the filesystem's own files.
            if !f.in_use || (n < FIRST_USER_RECORD && n != ROOT_RECORD) {
                continue;
            }
            let mut attributes = f.attributes & !win_attr::DIRECTORY;
            if f.dir {
                attributes |= win_attr::DIRECTORY;
            }
            let (size, alloc) = match f.data {
                _ if f.dir => (0, f.index_alloc),
                Some(d) => d,
                None => (0, 0),
            };
            let stat = WinStat {
                attributes,
                reparse_tag: if attributes & win_attr::REPARSE_POINT != 0 {
                    f.reparse_tag
                } else {
                    0
                },
                size,
                alloc,
                last_write: f.last_write,
                volume: 0,
                file_id: u128::from(u64::from(f.seq) << 48 | n),
                nlink: f.names.len() as u32,
            };
            for &(parent, off, len) in &f.names {
                let (pn, pseq) = (parent & 0xFFFF_FFFF_FFFF, (parent >> 48) as u16);
                // A name in a directory since reused for another file is stale.
                let live = files
                    .get(&pn)
                    .is_some_and(|p| p.in_use && p.dir && (pseq == 0 || p.seq == pseq));
                // The root's own name is in itself.
                if !live || (n == ROOT_RECORD && pn == ROOT_RECORD) {
                    continue;
                }
                entries.push(MftEntry {
                    parent: pn,
                    record: n,
                    name: (off, len),
                    stat,
                });
            }
        }
        entries.sort_unstable_by_key(|e| (e.parent, e.record, e.name.0));
        let mut starts = HashMap::new();
        let mut i = 0;
        while i < entries.len() {
            let p = entries[i].parent;
            let j = i + entries[i..].partition_point(|e| e.parent == p);
            starts.insert(p, (i as u32, j as u32));
            i = j;
        }
        let seqs = files
            .iter()
            .filter(|(_, f)| f.in_use && f.dir)
            .map(|(&n, f)| (n, f.seq))
            .collect();
        MftIndex {
            entries,
            starts,
            names,
            dirs: seqs,
        }
    }
}

/// One name of a file in a directory.
#[derive(Clone, Copy, Debug)]
pub struct MftEntry {
    pub parent: u64,
    pub record: u64,
    name: (u32, u16),
    pub stat: WinStat,
}

/// Every name in use on a volume, by directory.
pub struct MftIndex {
    entries: Vec<MftEntry>,
    /// The range of `entries` of each directory with entries.
    starts: HashMap<u64, (u32, u32)>,
    names: Vec<u16>,
    /// Directories in use and their sequence numbers.
    dirs: HashMap<u64, u16>,
}

impl MftIndex {
    /// The entries of the directory with file reference `dir` (record number and sequence
    /// number); `None` when no directory in use has that reference.
    #[must_use]
    pub fn children(&self, dir: u64) -> Option<&[MftEntry]> {
        let (n, seq) = (dir & 0xFFFF_FFFF_FFFF, (dir >> 48) as u16);
        if self.dirs.get(&n) != Some(&seq) {
            return None;
        }
        Some(match self.starts.get(&n) {
            Some(&(i, j)) => &self.entries[i as usize..j as usize],
            None => &[],
        })
    }

    #[must_use]
    pub fn name(&self, e: &MftEntry) -> &[u16] {
        &self.names[e.name.0 as usize..e.name.0 as usize + e.name.1 as usize]
    }

    /// The file references of every directory in use.
    pub fn dirs(&self) -> impl Iterator<Item = u64> + '_ {
        self.dirs.iter().map(|(&n, &seq)| u64::from(seq) << 48 | n)
    }

    /// Names in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds file records as NTFS writes them, update sequence included.
    pub struct Rec {
        buf: Vec<u8>,
        at: usize,
    }

    impl Rec {
        pub fn new(seq: u16, flags: u16, base: u64) -> Rec {
            let mut buf = vec![0u8; 1024];
            buf[..4].copy_from_slice(b"FILE");
            buf[4..6].copy_from_slice(&0x30u16.to_le_bytes());
            buf[6..8].copy_from_slice(&3u16.to_le_bytes());
            buf[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
            buf[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
            buf[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
            buf[0x1C..0x20].copy_from_slice(&1024u32.to_le_bytes());
            buf[0x20..0x28].copy_from_slice(&base.to_le_bytes());
            Rec { buf, at: 0x38 }
        }

        fn attr(&mut self, ty: u32, body: &[u8]) -> &mut Self {
            let len = body.len().div_ceil(8) * 8;
            let a = &mut self.buf[self.at..self.at + len];
            a[..body.len()].copy_from_slice(body);
            a[..4].copy_from_slice(&ty.to_le_bytes());
            a[4..8].copy_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
            self.at += len;
            self
        }

        fn resident(&mut self, ty: u32, name_len: u8, value: &[u8]) -> &mut Self {
            let mut body = vec![0u8; 0x18];
            body[9] = name_len;
            body[0x10..0x14].copy_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
            body[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
            body.extend(value);
            self.attr(ty, &body)
        }

        pub fn info(&mut self, attributes: u32, mtime: u64) -> &mut Self {
            let mut v = vec![0u8; 48];
            v[8..16].copy_from_slice(&mtime.to_le_bytes());
            v[32..36].copy_from_slice(&attributes.to_le_bytes());
            self.resident(AT_STANDARD_INFORMATION, 0, &v)
        }

        pub fn name(&mut self, parent: u64, name: &str, ns: u8) -> &mut Self {
            self.name_tagged(parent, name, ns, 0)
        }

        pub fn name_tagged(&mut self, parent: u64, name: &str, ns: u8, tag: u32) -> &mut Self {
            let units: Vec<u16> = name.encode_utf16().collect();
            let mut v = vec![0u8; 0x42];
            v[..8].copy_from_slice(&parent.to_le_bytes());
            if tag != 0 {
                v[0x38..0x3C].copy_from_slice(&win_attr::REPARSE_POINT.to_le_bytes());
                v[0x3C..0x40].copy_from_slice(&tag.to_le_bytes());
            }
            v[0x40] = u8::try_from(units.len()).unwrap();
            v[0x41] = ns;
            v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
            self.resident(AT_FILE_NAME, 0, &v)
        }

        pub fn small(&mut self, data: &[u8]) -> &mut Self {
            self.resident(AT_DATA, 0, data)
        }

        pub fn big(&mut self, ty: u32, flags: u16, alloc: u64, size: u64, total: u64) -> &mut Self {
            let mut body = vec![0u8; 0x48];
            body[8] = 1;
            body[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
            body[0x20..0x22].copy_from_slice(&0x48u16.to_le_bytes());
            // Last cluster: the one run below.
            body[0x18..0x20].copy_from_slice(&0u64.to_le_bytes());
            body[0x28..0x30].copy_from_slice(&alloc.to_le_bytes());
            body[0x30..0x38].copy_from_slice(&size.to_le_bytes());
            body[0x40..0x48].copy_from_slice(&total.to_le_bytes());
            // One run: 1 cluster at 100.
            body.extend([0x11, 1, 100, 0, 0, 0, 0, 0]);
            self.attr(ty, &body)
        }

        /// The record as on disk: end marker, used length and update sequence applied.
        pub fn bytes(&mut self) -> Vec<u8> {
            let at = self.at;
            self.buf[at..at + 4].copy_from_slice(&AT_END.to_le_bytes());
            self.buf[0x18..0x1C].copy_from_slice(&u32::try_from(at + 8).unwrap().to_le_bytes());
            let mut b = self.buf.clone();
            let check = [0x5A, 0xA5];
            b[0x30..0x32].copy_from_slice(&check);
            for i in 1..=2 {
                let end = i * STRIDE;
                b[0x30 + 2 * i] = b[end - 2];
                b[0x30 + 2 * i + 1] = b[end - 1];
                b[end - 2..end].copy_from_slice(&check);
            }
            b
        }
    }

    fn reference(n: u64, seq: u16) -> u64 {
        u64::from(seq) << 48 | n
    }

    #[test]
    fn boot_sectors_are_read() {
        let mut b = vec![0u8; 512];
        b[3..11].copy_from_slice(b"NTFS    ");
        b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        b[0x0D] = 8;
        b[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
        b[0x40] = 0xF6; // -10: 1024-byte records
        assert_eq!(
            Boot::parse(&b),
            Some(Boot {
                sector: 512,
                cluster: 4096,
                mft_lcn: 786_432,
                record: 1024
            })
        );
        b[0x0D] = 0xF4; // 2^12 sectors per cluster
        assert_eq!(Boot::parse(&b).map(|b| b.cluster), Some(2 << 20));
        b[0x0D] = 3;
        assert_eq!(Boot::parse(&b), None);
        b[0x0D] = 8;
        b[3] = b'X';
        assert_eq!(Boot::parse(&b), None);
    }

    #[test]
    fn data_runs_are_decoded() {
        // 0x18 clusters at 0x5634; 0x10 sparse; 0x20 at 0x5634 - 0x10.
        let b = [0x21, 0x18, 0x34, 0x56, 0x01, 0x10, 0x11, 0x20, 0xF0, 0x00];
        assert_eq!(
            data_runs(&b),
            Some(vec![
                Run {
                    lcn: Some(0x5634),
                    len: 0x18
                },
                Run {
                    lcn: None,
                    len: 0x10
                },
                Run {
                    lcn: Some(0x5624),
                    len: 0x20
                },
            ])
        );
        assert_eq!(data_runs(&[0x21, 0x18]), None);
        // Below cluster 0.
        assert_eq!(data_runs(&[0x11, 1, 0x80, 0]), None);
    }

    #[test]
    fn fixups_restore_sector_ends_and_catch_torn_writes() {
        let mut r = Rec::new(1, REC_IN_USE, 0);
        r.buf[510] = 0x11;
        r.buf[1023] = 0x22;
        let mut b = r.bytes();
        assert!(fixup(&mut b));
        assert_eq!((b[510], b[1023]), (0x11, 0x22));
        let mut torn = r.bytes();
        torn[1022] ^= 1;
        assert!(!fixup(&mut torn));
    }

    /// A small tree: the root, a directory, files resident and not, compressed and sparse
    /// sizes, a hard link, a DOS alias, a junction, a name in an extension record, a stale name
    /// and the filesystem's own files.
    pub fn volume() -> MftBuilder {
        let root = reference(ROOT_RECORD, 5);
        let mut b = MftBuilder::default();
        let mut add = |n: u64, r: &mut Rec| b.add(n, &mut r.bytes());
        add(
            0,
            Rec::new(1, REC_IN_USE, 0).info(6, 1).name(root, "$MFT", 3),
        );
        add(
            ROOT_RECORD,
            Rec::new(5, REC_IN_USE | REC_DIRECTORY, 0)
                .info(6, 1)
                .name(root, ".", 3),
        );
        let docs = reference(30, 2);
        add(
            30,
            Rec::new(2, REC_IN_USE | REC_DIRECTORY, 0)
                .info(0x10, 7)
                .name(root, "Documents", 1)
                .name(root, "DOCUME~1", NS_DOS)
                .big(AT_INDEX_ALLOCATION, 0, 4096, 4096, 0),
        );
        add(
            31,
            Rec::new(1, REC_IN_USE, 0)
                .info(0x20, 99)
                .name(docs, "note.txt", 3)
                .small(b"hello"),
        );
        add(
            32,
            Rec::new(9, REC_IN_USE, 0)
                .info(0x20, 98)
                .name(docs, "big.bin", 1)
                .name(root, "link.bin", 1)
                .big(AT_DATA, 0, 8192, 5000, 0),
        );
        add(
            33,
            Rec::new(1, REC_IN_USE, 0)
                .info(0x820, 97)
                .name(docs, "packed", 1)
                .big(AT_DATA, ATTR_COMPRESSED, 65536, 60000, 12288),
        );
        add(
            34,
            Rec::new(1, REC_IN_USE | REC_DIRECTORY, 0)
                .info(0x410, 96)
                .name_tagged(root, "junction", 1, 0xA000_0003),
        );
        // A second name in an extension record of 32.
        add(
            35,
            Rec::new(1, REC_IN_USE, reference(32, 9)).name(docs, "alias.bin", 1),
        );
        // A name in a directory record since reused (sequence 1, the directory is at 2).
        add(
            36,
            Rec::new(1, REC_IN_USE, 0)
                .info(0x20, 1)
                .name(reference(30, 1), "stale", 1)
                .small(b""),
        );
        // Deleted.
        add(37, Rec::new(1, 0, 0).info(0x20, 1).name(docs, "gone", 1));
        b
    }

    fn names(ix: &MftIndex, dir: u64) -> Vec<(String, WinStat)> {
        let mut v: Vec<_> = ix
            .children(dir)
            .unwrap()
            .iter()
            .map(|e| (String::from_utf16_lossy(ix.name(e)), e.stat))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    #[test]
    fn records_become_listings() {
        let b = volume();
        assert_eq!(b.damaged, 0);
        let ix = b.finish();
        let root = names(&ix, reference(ROOT_RECORD, 5));
        let root_names: Vec<&str> = root.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(root_names, ["Documents", "junction", "link.bin"]);
        let docs = &root[0].1;
        assert_eq!(docs.attributes & win_attr::DIRECTORY, win_attr::DIRECTORY);
        assert_eq!((docs.size, docs.alloc, docs.nlink), (0, 4096, 1));
        assert_eq!(docs.file_id, u128::from(reference(30, 2)));
        let j = &root[1].1;
        assert_eq!(j.reparse_tag, 0xA000_0003);
        assert_eq!(j.kind(), dfdu_tree::Kind::Symlink);

        let d = names(&ix, reference(30, 2));
        let dn: Vec<&str> = d.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(dn, ["alias.bin", "big.bin", "note.txt", "packed"]);
        let big = &d[1].1;
        assert_eq!(
            (big.size, big.alloc, big.nlink, big.last_write),
            (5000, 8192, 3, 98)
        );
        assert_eq!(d[0].1, *big);
        assert_eq!(root[2].1, *big);
        assert_eq!((d[2].1.size, d[2].1.alloc), (5, 8));
        assert_eq!((d[3].1.size, d[3].1.alloc), (60000, 12288));
        // Another sequence number is another directory.
        assert!(ix.children(reference(30, 1)).is_none());
        assert!(ix.children(reference(31, 1)).is_none());
    }

    #[test]
    fn the_mft_stream_is_found() {
        let mut r = Rec::new(1, REC_IN_USE, 0);
        r.info(6, 1)
            .name(reference(ROOT_RECORD, 5), "$MFT", 3)
            .big(AT_DATA, 0, 4096, 4096, 0);
        let mut b = r.bytes();
        assert!(fixup(&mut b));
        assert_eq!(
            mft_data(&b),
            Some((
                vec![Run {
                    lcn: Some(100),
                    len: 1
                }],
                4096
            ))
        );
        let mut r = Rec::new(1, REC_IN_USE, 0);
        r.small(b"resident");
        let mut b = r.bytes();
        assert!(fixup(&mut b));
        assert_eq!(mft_data(&b), None);
    }

    /// No cut, bit flip or overwrite of a record panics.
    #[test]
    fn damaged_records_never_panic() {
        let mut r = Rec::new(3, REC_IN_USE, 0);
        r.info(0x20, 5).name(reference(ROOT_RECORD, 5), "x", 1).big(
            AT_DATA,
            ATTR_SPARSE,
            4096,
            100,
            0,
        );
        let good = r.bytes();
        let mut b = MftBuilder::default();
        for i in 0..good.len() * 8 {
            let mut rec = good.clone();
            rec[i / 8] ^= 1 << (i % 8);
            b.add(40, &mut rec);
            if fixup(&mut rec.clone()) {
                let _ = mft_data(&rec);
            }
        }
        for i in 0..good.len() {
            let mut rec = good.clone();
            rec[i] = 0xFF;
            b.add(41, &mut rec);
            let mut cut = good[..i - i % STRIDE].to_vec();
            b.add(42, &mut cut);
        }
        let _ = b.finish();
    }
}
