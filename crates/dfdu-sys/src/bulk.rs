//! Records written by macOS `getattrlistbulk(2)`, parsed in safe code. Built on every system
//! for its tests; only the macOS backend uses it.
//!
//! The buffer holds one record per directory entry. A record starts with its length (`u32`),
//! then the attributes that were returned, in a fixed order: `ATTR_CMN_RETURNED_ATTRS` (an
//! `attribute_set_t` saying which of the requested attributes follow), `ATTR_CMN_ERROR`, the
//! other common attributes in bit order, then the file attributes in bit order. An attribute
//! the filesystem did not return takes no space (the call is made without
//! `FSOPT_PACK_INVAL_ATTRS`), so each field's position depends on the bits before it.
//! Variable-length data (the name) follows the fixed part and is located by an
//! `attrreference_t` (offset from the reference itself, length including the NUL). Fields are
//! only 4-byte aligned, so 8-byte values are read with `from_ne_bytes`, never through pointers.
//!
//! An entry whose attributes could not be read comes back as a record with only the returned
//! set, the error and the name (XNU `get_error_attributes`).

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io;

// Attribute bits from <sys/attr.h>. Defined here rather than taken from `libc` because the
// parser is also built (for its tests) where `libc` has no Apple constants, and because
// `libc` 0.2.191 lacks `ATTR_CMN_ERROR`; the macOS module checks the others against `libc`.
pub const ATTR_BIT_MAP_COUNT: u16 = 5;
pub const ATTR_CMN_NAME: u32 = 0x0000_0001;
pub const ATTR_CMN_DEVID: u32 = 0x0000_0002;
pub const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
pub const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
pub const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
pub const ATTR_CMN_FILEID: u32 = 0x0200_0000;
pub const ATTR_CMN_ERROR: u32 = 0x2000_0000;
pub const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
pub const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
pub const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
pub const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;

/// The common attributes [`crate::read_bulk`] requests.
pub const BULK_COMMON: u32 = ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | ATTR_CMN_NAME
    | ATTR_CMN_DEVID
    | ATTR_CMN_OBJTYPE
    | ATTR_CMN_MODTIME
    | ATTR_CMN_FLAGS
    | ATTR_CMN_FILEID;
/// The file attributes [`crate::read_bulk`] requests. Filesystems return them for entries
/// that are not directories; `ATTR_FILE_ALLOCSIZE` covers all forks, as `st_blocks` does.
pub const BULK_FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH;

// `fsobj_type_t` values (`enum vtype` in <sys/vnode.h>).
pub const VREG: u32 = 1;
pub const VDIR: u32 = 2;
pub const VLNK: u32 = 5;

/// `st_flags` bit of a firmlink (<sys/stat.h>; macOS 10.15).
pub const SF_FIRMLINK: u32 = 0x0080_0000;

/// One entry as `getattrlistbulk` reported it. Fields are `None` when the filesystem did not
/// return that attribute for this entry.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BulkEntry<'a> {
    /// The name, without its NUL.
    pub name: &'a [u8],
    /// `errno` of the attributes that could not be read (`ATTR_CMN_ERROR`); 0 when none.
    pub error: u32,
    /// `fsobj_type_t`: [`VREG`], [`VDIR`], [`VLNK`] and others.
    pub obj_type: Option<u32>,
    /// `ATTR_CMN_DEVID`: the `dev_t` that `stat` reports as `st_dev`.
    pub dev: Option<i32>,
    /// `ATTR_CMN_FILEID`: the inode number.
    pub ino: Option<u64>,
    /// `ATTR_CMN_MODTIME` as `(seconds, nanoseconds)`.
    pub mtime: Option<(i64, i64)>,
    /// `ATTR_CMN_FLAGS`: `st_flags`.
    pub flags: Option<u32>,
    /// `ATTR_FILE_LINKCOUNT`.
    pub nlink: Option<u32>,
    /// `ATTR_FILE_ALLOCSIZE`: bytes allocated for all forks.
    pub alloc: Option<i64>,
    /// `ATTR_FILE_DATALENGTH`: length of the data fork, `st_size`.
    pub size: Option<i64>,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("getattrlistbulk record: {what}"),
    )
}

/// Reads fixed-size fields from one record, bounds-checked.
struct Cursor<'a> {
    rec: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let b = self
            .rec
            .get(self.pos..self.pos + N)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| bad("field past the end"))?;
        self.pos += N;
        Ok(b)
    }

    fn u32(&mut self) -> io::Result<u32> {
        self.take::<4>().map(u32::from_ne_bytes)
    }

    fn i32(&mut self) -> io::Result<i32> {
        self.take::<4>().map(i32::from_ne_bytes)
    }

    fn u64(&mut self) -> io::Result<u64> {
        self.take::<8>().map(u64::from_ne_bytes)
    }

    fn i64(&mut self) -> io::Result<i64> {
        self.take::<8>().map(i64::from_ne_bytes)
    }

    /// An `attrreference_t` and the bytes it points at.
    fn reference(&mut self) -> io::Result<&'a [u8]> {
        let at = self.pos;
        let off = self.i32()?;
        let len = self.u32()? as usize;
        let start = isize::try_from(at)
            .ok()
            .and_then(|a| a.checked_add(off as isize))
            .and_then(|s| usize::try_from(s).ok())
            .ok_or_else(|| bad("reference before the record"))?;
        start
            .checked_add(len)
            .and_then(|end| self.rec.get(start..end))
            .ok_or_else(|| bad("reference past the end"))
    }
}

/// Parses the record at byte `pos` of `buf`, which holds what one `getattrlistbulk` call
/// returned for `BULK_COMMON` and `BULK_FILE`. Returns the entry and the position of the
/// next record.
///
/// # Errors
///
/// `InvalidData` if the record does not fit in `buf` or contradicts itself.
pub fn parse_bulk_record(buf: &[u8], pos: usize) -> io::Result<(BulkEntry<'_>, usize)> {
    let len_bytes: [u8; 4] = buf
        .get(pos..pos + 4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| bad("length past the end"))?;
    let len = u32::from_ne_bytes(len_bytes) as usize;
    let rec = pos
        .checked_add(len)
        .and_then(|end| buf.get(pos..end))
        .filter(|_| len >= 4 + 20)
        .ok_or_else(|| bad("bad length"))?;
    let mut c = Cursor { rec, pos: 4 };
    // `attribute_set_t`: common, volume, directory, file and fork attributes.
    let common = c.u32()?;
    let _vol = c.u32()?;
    let _dir = c.u32()?;
    let file = c.u32()?;
    let _fork = c.u32()?;
    let mut e = BulkEntry::default();
    if common & ATTR_CMN_ERROR != 0 {
        e.error = c.u32()?;
    }
    if common & ATTR_CMN_NAME == 0 {
        return Err(bad("no name"));
    }
    let name = c.reference()?;
    e.name = name.strip_suffix(&[0]).unwrap_or(name);
    if e.name.is_empty() || e.name.contains(&0) {
        return Err(bad("bad name"));
    }
    if common & ATTR_CMN_DEVID != 0 {
        e.dev = Some(c.i32()?);
    }
    if common & ATTR_CMN_OBJTYPE != 0 {
        e.obj_type = Some(c.u32()?);
    }
    if common & ATTR_CMN_MODTIME != 0 {
        // `struct timespec` of a 64-bit process: two 8-byte fields.
        e.mtime = Some((c.i64()?, c.i64()?));
    }
    if common & ATTR_CMN_FLAGS != 0 {
        e.flags = Some(c.u32()?);
    }
    if common & ATTR_CMN_FILEID != 0 {
        e.ino = Some(c.u64()?);
    }
    if file & ATTR_FILE_LINKCOUNT != 0 {
        e.nlink = Some(c.u32()?);
    }
    if file & ATTR_FILE_ALLOCSIZE != 0 {
        e.alloc = Some(c.i64()?);
    }
    if file & ATTR_FILE_DATALENGTH != 0 {
        e.size = Some(c.i64()?);
    }
    Ok((e, pos + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds records the way XNU packs them.
    #[derive(Default)]
    struct Rec {
        common: u32,
        file: u32,
        fixed: Vec<u8>,
    }

    impl Rec {
        fn new() -> Self {
            Rec {
                common: ATTR_CMN_RETURNED_ATTRS,
                ..Default::default()
            }
        }
        fn field(mut self, bit: u32, file: bool, bytes: &[u8]) -> Self {
            if file {
                self.file |= bit;
            } else {
                self.common |= bit;
            }
            self.fixed.extend_from_slice(bytes);
            self
        }
        /// The name reference goes where the name field is; the name itself after the fixed
        /// part, padded to 4 bytes.
        fn build(self, name_at: Option<(usize, &[u8])>) -> Vec<u8> {
            let mut fixed = self.fixed;
            let head = 4 + 20;
            let mut var = Vec::new();
            if let Some((at, name)) = name_at {
                let fixed_len = fixed.len() + 8;
                let mut r = Vec::new();
                let off =
                    i32::try_from(head + fixed_len).unwrap() - i32::try_from(head + at).unwrap();
                r.extend_from_slice(&off.to_ne_bytes());
                r.extend_from_slice(&(u32::try_from(name.len()).unwrap() + 1).to_ne_bytes());
                fixed.splice(at..at, r);
                var.extend_from_slice(name);
                var.push(0);
                while var.len() % 4 != 0 {
                    var.push(0);
                }
            }
            let len = head + fixed.len() + var.len();
            let mut out = Vec::new();
            out.extend_from_slice(&u32::try_from(len).unwrap().to_ne_bytes());
            let common = self.common | if name_at.is_some() { ATTR_CMN_NAME } else { 0 };
            for w in [common, 0, 0, self.file, 0] {
                out.extend_from_slice(&w.to_ne_bytes());
            }
            out.extend_from_slice(&fixed);
            out.extend_from_slice(&var);
            out
        }
    }

    fn file_record(name: &[u8]) -> Vec<u8> {
        // The name reference comes first among the fixed fields after the error.
        Rec::new()
            .field(ATTR_CMN_DEVID, false, &16_777_234i32.to_ne_bytes())
            .field(ATTR_CMN_OBJTYPE, false, &VREG.to_ne_bytes())
            .field(
                ATTR_CMN_MODTIME,
                false,
                &[1_700_000_000i64.to_ne_bytes(), 5i64.to_ne_bytes()].concat(),
            )
            .field(ATTR_CMN_FLAGS, false, &0u32.to_ne_bytes())
            .field(ATTR_CMN_FILEID, false, &0x1234_5678_9abcu64.to_ne_bytes())
            .field(ATTR_FILE_LINKCOUNT, true, &2u32.to_ne_bytes())
            .field(ATTR_FILE_ALLOCSIZE, true, &8192i64.to_ne_bytes())
            .field(ATTR_FILE_DATALENGTH, true, &5000i64.to_ne_bytes())
            .build(Some((0, name)))
    }

    #[test]
    fn records_in_sequence() {
        let mut buf = file_record(b"a.txt");
        // A directory: no file attributes, a firmlink flag.
        let dir = Rec::new()
            .field(ATTR_CMN_DEVID, false, &(-5i32).to_ne_bytes())
            .field(ATTR_CMN_OBJTYPE, false, &VDIR.to_ne_bytes())
            .field(
                ATTR_CMN_MODTIME,
                false,
                &[(-1i64).to_ne_bytes(), 0i64.to_ne_bytes()].concat(),
            )
            .field(ATTR_CMN_FLAGS, false, &SF_FIRMLINK.to_ne_bytes())
            .field(ATTR_CMN_FILEID, false, &77u64.to_ne_bytes())
            .build(Some((0, b"Users")));
        buf.extend_from_slice(&dir);
        // An entry whose attributes could not be read.
        let err = Rec::new()
            .field(ATTR_CMN_ERROR, false, &13u32.to_ne_bytes())
            .build(Some((4, b"locked")));
        buf.extend_from_slice(&err);
        // A record without the type: the caller has to stat it.
        let partial = Rec::new()
            .field(ATTR_CMN_FILEID, false, &9u64.to_ne_bytes())
            .build(Some((0, "é".as_bytes())));
        buf.extend_from_slice(&partial);

        let (a, next) = parse_bulk_record(&buf, 0).unwrap();
        assert_eq!(
            a,
            BulkEntry {
                name: b"a.txt",
                error: 0,
                obj_type: Some(VREG),
                dev: Some(16_777_234),
                ino: Some(0x1234_5678_9abc),
                mtime: Some((1_700_000_000, 5)),
                flags: Some(0),
                nlink: Some(2),
                alloc: Some(8192),
                size: Some(5000),
            }
        );
        // The 8-byte fields of the first record are at offsets that are not multiples of 8.
        assert_eq!(next, file_record(b"a.txt").len());
        let (d, next) = parse_bulk_record(&buf, next).unwrap();
        assert_eq!(d.name, b"Users");
        assert_eq!(
            (d.obj_type, d.dev, d.flags, d.ino, d.mtime),
            (
                Some(VDIR),
                Some(-5),
                Some(SF_FIRMLINK),
                Some(77),
                Some((-1, 0))
            )
        );
        assert_eq!((d.nlink, d.alloc, d.size), (None, None, None));
        let (e, next) = parse_bulk_record(&buf, next).unwrap();
        assert_eq!((e.name, e.error, e.obj_type), (&b"locked"[..], 13, None));
        let (p, next) = parse_bulk_record(&buf, next).unwrap();
        assert_eq!((p.name, p.ino, p.obj_type), ("é".as_bytes(), Some(9), None));
        assert_eq!(next, buf.len());
    }

    #[test]
    fn malformed_records_are_errors() {
        let good = file_record(b"name");
        // Truncated anywhere: past the length, the attribute set or a field.
        for cut in [0, 2, 10, 30, good.len() - 1] {
            let e = parse_bulk_record(&good[..cut], 0).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "cut at {cut}");
        }
        // A length too short to hold the attribute set.
        let mut short = good.clone();
        short[..4].copy_from_slice(&8u32.to_ne_bytes());
        assert!(parse_bulk_record(&short, 0).is_err());
        // A name reference pointing past the record, or before it.
        for off in [10_000i32, -100] {
            let mut r = good.clone();
            r[24..28].copy_from_slice(&off.to_ne_bytes());
            assert!(parse_bulk_record(&r, 0).is_err(), "offset {off}");
        }
        // No name returned.
        let mut r = good.clone();
        let common = u32::from_ne_bytes(r[4..8].try_into().unwrap()) & !ATTR_CMN_NAME;
        r[4..8].copy_from_slice(&common.to_ne_bytes());
        assert!(parse_bulk_record(&r, 0).is_err());
        // A length of zero would loop forever if accepted.
        assert!(parse_bulk_record(&[0u8; 64], 0).is_err());
    }
}
