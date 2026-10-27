//! macOS calls without a safe binding: `getattrlistbulk(2)`.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::bulk::{
    ATTR_BIT_MAP_COUNT, ATTR_CMN_DEVID, ATTR_CMN_FILEID, ATTR_CMN_FLAGS, ATTR_CMN_MODTIME,
    ATTR_CMN_NAME, ATTR_CMN_OBJTYPE, ATTR_CMN_RETURNED_ATTRS, ATTR_FILE_ALLOCSIZE,
    ATTR_FILE_DATALENGTH, ATTR_FILE_LINKCOUNT, BULK_COMMON, BULK_FILE,
};

// The constants `libc` also has must agree with it.
const _: () = {
    assert!(ATTR_BIT_MAP_COUNT == libc::ATTR_BIT_MAP_COUNT);
    assert!(ATTR_CMN_NAME == libc::ATTR_CMN_NAME);
    assert!(ATTR_CMN_DEVID == libc::ATTR_CMN_DEVID);
    assert!(ATTR_CMN_OBJTYPE == libc::ATTR_CMN_OBJTYPE);
    assert!(ATTR_CMN_MODTIME == libc::ATTR_CMN_MODTIME);
    assert!(ATTR_CMN_FLAGS == libc::ATTR_CMN_FLAGS);
    assert!(ATTR_CMN_FILEID == libc::ATTR_CMN_FILEID);
    assert!(ATTR_CMN_RETURNED_ATTRS == libc::ATTR_CMN_RETURNED_ATTRS);
    assert!(ATTR_FILE_LINKCOUNT == libc::ATTR_FILE_LINKCOUNT);
    assert!(ATTR_FILE_ALLOCSIZE == libc::ATTR_FILE_ALLOCSIZE);
    assert!(ATTR_FILE_DATALENGTH == libc::ATTR_FILE_DATALENGTH);
    // The record layout in `bulk` assumes a 64-bit `struct timespec`.
    assert!(std::mem::size_of::<libc::timespec>() == 16);
};

/// Reads the next entries of the open directory `dir` into `buf` with `getattrlistbulk`,
/// asking for `BULK_COMMON` and `BULK_FILE`. Returns the number of records written (parse
/// them with [`crate::parse_bulk_record`]); 0 at the end of the directory. Retries on `EINTR`.
///
/// Each call continues where the previous one on the same descriptor stopped. Entries `.` and
/// `..` are not returned.
#[allow(unsafe_code)]
pub fn read_bulk(dir: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<u32> {
    let mut list = libc::attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: BULK_COMMON,
        volattr: 0,
        dirattr: 0,
        fileattr: BULK_FILE,
        forkattr: 0,
    };
    loop {
        // SAFETY: `list` is a valid `struct attrlist` that the kernel only reads. `buf` is
        // valid for writes of `buf.len()` bytes, the size passed, and any bytes the kernel
        // writes are valid `u8`s. `dir` is an open descriptor for the duration of the call.
        // No `FSOPT_*` options: the records then hold only the attributes returned, the
        // layout `parse_bulk_record` reads.
        let n = unsafe {
            libc::getattrlistbulk(
                dir.as_raw_fd(),
                (&raw mut list).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if n >= 0 {
            return Ok(n as u32);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bulk::{SF_FIRMLINK, VDIR, VLNK, VREG, parse_bulk_record};
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn records_match_lstat() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("file"), vec![7u8; 10_000]).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::os::unix::fs::symlink("file", root.join("link")).unwrap();
        let dir = std::fs::File::open(root).unwrap();
        let mut buf = vec![0u8; 128 * 1024];
        let mut seen = Vec::new();
        loop {
            let n = read_bulk(dir.as_fd(), &mut buf).unwrap();
            if n == 0 {
                break;
            }
            let mut pos = 0;
            for _ in 0..n {
                let (e, next) = parse_bulk_record(&buf, pos).unwrap();
                pos = next;
                let name = std::str::from_utf8(e.name).unwrap().to_string();
                let md = std::fs::symlink_metadata(root.join(&name)).unwrap();
                assert_eq!(e.error, 0, "{name}");
                assert_eq!(e.dev.map(|d| d as u64), Some(md.dev()), "{name}: dev");
                assert_eq!(e.ino, Some(md.ino()), "{name}: ino");
                assert_eq!(e.mtime.map(|t| t.0), Some(md.mtime()), "{name}: mtime");
                assert_eq!(e.flags.unwrap() & SF_FIRMLINK, 0);
                let want = match name.as_str() {
                    "dir" => VDIR,
                    "link" => VLNK,
                    _ => VREG,
                };
                assert_eq!(e.obj_type, Some(want), "{name}: type");
                if want != VDIR {
                    assert_eq!(e.nlink, Some(md.nlink() as u32), "{name}: nlink");
                    assert_eq!(e.size, Some(md.size() as i64), "{name}: size");
                    let alloc = e.alloc.unwrap() as u64;
                    assert_eq!(alloc.div_ceil(512), md.blocks(), "{name}: blocks");
                }
                seen.push(name);
            }
        }
        seen.sort();
        assert_eq!(seen, ["dir", "file", "link"]);
    }
}
