//! macOS calls without a safe binding: `getattrlistbulk(2)`, and `fgetattrlist(2)` for APFS
//! clone information. The `FSEvents` history is in [`fsevents`].

pub mod fsevents;

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
///
/// # Errors
///
/// The error `getattrlistbulk` reports, such as `ENOTDIR` or `EBADF`.
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
            return Ok(n.cast_unsigned());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Performance cores (`hw.perflevel0.physicalcpu`) on a chip with more than one kind; `None`
/// when all cores are alike or the system does not say.
#[allow(unsafe_code)]
#[must_use]
pub fn performance_cores() -> Option<usize> {
    let get = |name: &std::ffi::CStr| -> Option<u32> {
        let mut v: u32 = 0;
        let mut len = std::mem::size_of::<u32>();
        // SAFETY: `name` is NUL-terminated; `v` is valid for writes of `len` bytes, the size
        // passed; no new value is set.
        let r = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut v).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (r == 0 && len == std::mem::size_of::<u32>()).then_some(v)
    };
    if get(c"hw.nperflevels")? < 2 {
        return None;
    }
    get(c"hw.perflevel0.physicalcpu")
        .filter(|&n| n > 0)
        .map(|n| n as usize)
}

/// What an APFS file shares with its clones.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CloneInfo {
    /// Bytes of the file no other file shares: what deleting it frees.
    pub private_size: u64,
    /// Identifies the file's data stream; clones made from the same file start with the same
    /// id.
    pub clone_id: u64,
}

/// The clone information of the open file `f`, from `ATTR_CMNEXT_PRIVATESIZE` and
/// `ATTR_CMNEXT_CLONEID`; `None` when the filesystem does not report them (not APFS).
#[allow(unsafe_code)]
#[must_use]
pub fn clone_info(f: BorrowedFd<'_>) -> Option<CloneInfo> {
    let mut list = libc::attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID,
    };
    // Length, the returned attribute set (five u32), then the attributes in bit order.
    let mut buf = [0u8; 4 + 20 + 8 + 8];
    // SAFETY: `list` is a valid `struct attrlist` the kernel only reads; `buf` is valid for
    // writes of its length, the size passed, and any bytes written are valid `u8`s; `f` is an
    // open descriptor for the duration of the call.
    let r = unsafe {
        libc::fgetattrlist(
            f.as_raw_fd(),
            (&raw mut list).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if r != 0 {
        return None;
    }
    parse_clone_info(&buf)
}

/// What [`clone_info_at`] found: the file's inode and allocation with its clone information.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CloneAt {
    pub ino: u64,
    /// `ATTR_FILE_ALLOCSIZE`, as the scan reads it.
    pub alloc: u64,
    pub info: CloneInfo,
}

/// [`clone_info`] for `name` in the directory `dir`, without opening the file (a symlink is not
/// followed), with its inode number to check it is still the file scanned. `Unsupported` when
/// the filesystem does not report clone information (not APFS).
///
/// # Errors
///
/// The error `getattrlistat` reports (`ENOENT` when `name` is gone), or `Unsupported` when
/// the filesystem does not report clone information.
#[allow(unsafe_code)]
pub fn clone_info_at(dir: BorrowedFd<'_>, name: &std::ffi::CStr) -> io::Result<CloneAt> {
    let mut list = libc::attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_ALLOCSIZE,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID,
    };
    // Length, the returned attribute set (five u32), then the attributes in order: common,
    // file, then the extended common ones.
    let mut buf = [0u8; 4 + 20 + 8 + 8 + 8 + 8];
    // SAFETY: `list` is a valid `struct attrlist` the kernel only reads; `name` is
    // NUL-terminated; `buf` is valid for writes of its length, the size passed, and any bytes
    // written are valid `u8`s; `dir` is an open descriptor for the duration of the call.
    let r = unsafe {
        libc::getattrlistat(
            dir.as_raw_fd(),
            name.as_ptr(),
            (&raw mut list).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            (libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW).into(),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    parse_clone_at(&buf).ok_or_else(|| io::ErrorKind::Unsupported.into())
}

/// The native-endian `u32` at byte `i` of `buf`, if `buf` holds it.
fn u32_at(buf: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(buf.get(i..i + 4)?.try_into().ok()?))
}

/// The native-endian `u64` at byte `i` of `buf`, if `buf` holds it.
fn u64_at(buf: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(buf.get(i..i + 8)?.try_into().ok()?))
}

/// Reads [`clone_info_at`]'s buffer: every attribute must have been returned.
fn parse_clone_at(buf: &[u8]) -> Option<CloneAt> {
    let (common, file, fork) = (u32_at(buf, 4)?, u32_at(buf, 4 + 12)?, u32_at(buf, 4 + 16)?);
    let want = libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID;
    if common & ATTR_CMN_FILEID == 0
        || file & ATTR_FILE_ALLOCSIZE == 0
        || fork & want != want
        || (u32_at(buf, 0)? as usize) < buf.len()
    {
        return None;
    }
    Some(CloneAt {
        ino: u64_at(buf, 24)?,
        alloc: u64_at(buf, 32)?,
        info: CloneInfo {
            private_size: u64_at(buf, 40)?,
            clone_id: u64_at(buf, 48)?,
        },
    })
}

/// Whether the filesystem at `path` is APFS, whose clones share data.
///
/// # Errors
///
/// `InvalidInput` when `path` holds a NUL byte, or the error `statfs` reports.
#[allow(unsafe_code)]
pub fn shares_extents(path: &std::path::Path) -> io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `statfs` holds only integers and integer arrays, so all zeros is a valid value.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` is valid for writes of a `statfs`.
    if unsafe { libc::statfs(c.as_ptr(), &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let name: Vec<u8> = st
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.cast_unsigned())
        .collect();
    Ok(name == b"apfs")
}

/// The text of a NUL-terminated `statfs` name field.
fn field(chars: &[libc::c_char]) -> Vec<u8> {
    chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.cast_unsigned())
        .collect()
}

/// What the kernel last recorded of each mounted filesystem (`getfsstat` with `MNT_NOWAIT`),
/// in mount order. No filesystem is asked, so a network mount that does not answer is listed
/// like any other.
#[allow(unsafe_code)]
fn fsstat() -> io::Result<Vec<libc::statfs>> {
    // SAFETY: a null buffer only asks for the number of mounted filesystems.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let too_many = || io::Error::other("too many mounted filesystems");
    // Room for a few mounted in between.
    let cap = usize::try_from(n).map_err(|_| too_many())? + 8;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(cap);
    let bytes =
        libc::c_int::try_from(cap * std::mem::size_of::<libc::statfs>()).map_err(|_| too_many())?;
    // SAFETY: `buf` has room for `cap` records, `bytes` in all; getfsstat writes at most that
    // and returns how many records it wrote.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    let Ok(got) = usize::try_from(got) else {
        return Err(io::Error::last_os_error());
    };
    // SAFETY: the first `got` records (at most `cap`) were written.
    unsafe { buf.set_len(got.min(cap)) };
    Ok(buf)
}

/// The mounted filesystems: mount point and type, in mount order ([`fsstat`]).
pub(crate) fn mounted() -> io::Result<Vec<(std::path::PathBuf, String)>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(fsstat()?
        .iter()
        .map(|s| {
            let at = std::ffi::OsStr::from_bytes(&field(&s.f_mntonname)).to_owned();
            let fstype = String::from_utf8_lossy(&field(&s.f_fstypename)).into_owned();
            (at.into(), fstype)
        })
        .collect())
}

/// `MNT_EXT_ROOT_DATA_VOL` of `<sys/mount.h>`, in `f_flags_ext`: the data volume of the volume
/// group the system started from.
const MNT_EXT_ROOT_DATA_VOL: u32 = 0x1;

/// The mounted volumes with their devices, types and roles, in mount order ([`fsstat`]): the
/// read-only root is the system volume, and the kernel marks its data volume.
pub(crate) fn volumes() -> io::Result<Vec<crate::disks::Volume>> {
    use std::os::unix::ffi::OsStrExt;
    let system = (libc::MNT_ROOTFS | libc::MNT_RDONLY).cast_unsigned();
    let flag = |s: &libc::statfs, f: libc::c_int| s.f_flags & f.cast_unsigned() != 0;
    Ok(fsstat()?
        .iter()
        .map(|s| crate::disks::Volume {
            at: std::ffi::OsStr::from_bytes(&field(&s.f_mntonname))
                .to_owned()
                .into(),
            from: String::from_utf8_lossy(&field(&s.f_mntfromname)).into_owned(),
            fstype: String::from_utf8_lossy(&field(&s.f_fstypename)).into_owned(),
            role: if s.f_flags & system == system {
                Some(crate::Role::System)
            } else if s.f_flags_ext & MNT_EXT_ROOT_DATA_VOL != 0 {
                Some(crate::Role::Data)
            } else {
                None
            },
            browsable: !flag(s, libc::MNT_DONTBROWSE),
            local: flag(s, libc::MNT_LOCAL),
        })
        .collect())
}

/// Size and use of the filesystem at `path` (`statfs`).
#[allow(unsafe_code)]
pub(crate) fn statfs_space(path: &std::path::Path) -> io::Result<crate::Space> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `statfs` is plain data, valid when zeroed.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and `st` is valid for writes of a `statfs`.
    if unsafe { libc::statfs(c.as_ptr(), &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let unit = u64::from(st.f_bsize);
    Ok(crate::Space {
        total: st.f_blocks.saturating_mul(unit),
        used: st.f_blocks.saturating_sub(st.f_bfree).saturating_mul(unit),
        available: st.f_bavail.saturating_mul(unit),
    })
}

/// Reads [`clone_info`]'s buffer: both attributes must have been returned.
fn parse_clone_info(buf: &[u8]) -> Option<CloneInfo> {
    // attribute_set_t: commonattr, volattr, dirattr, fileattr, forkattr.
    let fork = u32_at(buf, 4 + 16)?;
    let want = libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID;
    if fork & want != want || (u32_at(buf, 0)? as usize) < buf.len() {
        return None;
    }
    Some(CloneInfo {
        private_size: u64_at(buf, 24)?,
        clone_id: u64_at(buf, 32)?,
    })
}

#[cfg(test)]
mod tests;
