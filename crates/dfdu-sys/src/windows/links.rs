//! Link counts and sizes of files by name in an open directory.

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN_REPARSE_POINT, FILE_STAT_INFORMATION, FileStandardInformation, FileStatInformation,
    NtQueryInformationByName, NtQueryInformationFile,
};
use windows_sys::Win32::Foundation::{
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER,
    STATUS_NOT_IMPLEMENTED, STATUS_NOT_SUPPORTED,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, SYNCHRONIZE};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{StandardInfo, len_u32, nt_error, nt_open, object_attributes, unicode};

/// What [`LinkQuery::query`] returns for a file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Links {
    pub nlink: u32,
    /// `EndOfFile` and `AllocationSize` from the file itself. A directory entry's copy of them
    /// can be out of date for a file with several names: NTFS updates the entry of the name
    /// the file was changed through.
    pub size: u64,
    pub alloc: u64,
}

/// Link counts of files by name in an open directory, which the directory enumeration does
/// not report. `NtQueryInformationByName` with `FileStatInformation` (Windows 10 1709 and
/// later; one call, no handle) where the filesystem supports it, else a relative
/// `NtCreateFile` with attribute access, `FileStandardInformation` and a close. Neither reads
/// file data.
#[derive(Debug)]
pub struct LinkQuery {
    /// `NtQueryInformationByName` has not been refused yet.
    by_name: AtomicBool,
}

impl Default for LinkQuery {
    fn default() -> Self {
        LinkQuery {
            by_name: AtomicBool::new(true),
        }
    }
}

impl LinkQuery {
    /// Link count and sizes of the file `name` (UTF-16, one component) in `dir`. A reparse
    /// point at `name` is never followed: such files (`reparse`: deduplicated, WOF-compressed)
    /// are opened with `FILE_OPEN_REPARSE_POINT`, as [`stat_path`](super::stat_path) does.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when `name` is not one component, else the error of opening or querying
    /// the file (`NotFound` when it is gone).
    pub fn query(&self, dir: BorrowedHandle<'_>, name: &[u16], reparse: bool) -> io::Result<Links> {
        if name.is_empty() || name.contains(&u16::from(b'\\')) || name.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if !reparse && self.by_name.load(Ordering::Relaxed) {
            match by_name(dir, name)? {
                Some(l) => return Ok(l),
                None => self.by_name.store(false, Ordering::Relaxed),
            }
        }
        let h = nt_open(
            Some(dir),
            name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN_REPARSE_POINT,
        )?;
        let mut iosb = IO_STATUS_BLOCK::default();
        let mut info = StandardInfo::default();
        // SAFETY: `info` is valid for writes of the size passed, and any bytes written form a
        // valid `StandardInfo` (`Plain`); `iosb` is valid for writes.
        let st = unsafe {
            NtQueryInformationFile(
                h.as_raw_handle(),
                &raw mut iosb,
                (&raw mut info).cast::<c_void>(),
                len_u32(size_of::<StandardInfo>()),
                FileStandardInformation,
            )
        };
        if st < 0 {
            return Err(nt_error(st));
        }
        Ok(Links {
            nlink: info.number_of_links,
            size: info.end_of_file.cast_unsigned(),
            alloc: info.allocation_size.cast_unsigned(),
        })
    }
}

/// `NtQueryInformationByName`; `Ok(None)` when the call or class is not supported.
fn by_name(dir: BorrowedHandle<'_>, name: &[u16]) -> io::Result<Option<Links>> {
    let us = unicode(name)?;
    let oa = object_attributes(Some(dir), &us, 0);
    let mut iosb = IO_STATUS_BLOCK::default();
    let mut info = FILE_STAT_INFORMATION::default();
    // SAFETY: `oa` points to `us`, which points into `name`; both outlive the call. `info` is
    // valid for writes of the size passed and any bytes written form a valid value (`Plain`);
    // `iosb` is valid for writes.
    let st = unsafe {
        NtQueryInformationByName(
            &raw const oa,
            &raw mut iosb,
            (&raw mut info).cast::<c_void>(),
            len_u32(size_of::<FILE_STAT_INFORMATION>()),
            FileStatInformation,
        )
    };
    match st {
        s if s >= 0 => Ok(Some(Links {
            nlink: info.NumberOfLinks,
            size: info.EndOfFile.cast_unsigned(),
            alloc: info.AllocationSize.cast_unsigned(),
        })),
        STATUS_INVALID_INFO_CLASS
        | STATUS_NOT_IMPLEMENTED
        | STATUS_NOT_SUPPORTED
        | STATUS_INVALID_PARAMETER
        | STATUS_INVALID_DEVICE_REQUEST => Ok(None),
        s => Err(nt_error(s)),
    }
}

#[cfg(test)]
mod tests;
