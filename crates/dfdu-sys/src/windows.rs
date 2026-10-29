//! Windows calls behind safe wrappers: directory enumeration with sizes
//! (`GetFileInformationByHandleEx` with `FileIdExtdDirectoryInfo`), link counts by name
//! relative to an open directory (`NtQueryInformationByName`), and the metadata of one path
//! from an open handle, as [`dfdu_tree::WinStat`].
//!
//! Paths are opened with `NtCreateFile` in the NT namespace (`\??\C:\dir`), so they are not
//! limited to `MAX_PATH`; a path already in verbatim form (`\\?\`, see [`verbatim_path`]) is
//! not normalised either, so names ending in a dot or a space can be opened.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use dfdu_tree::{WinStat, win_attr};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT,
    FILE_STANDARD_INFORMATION, FILE_STAT_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_DELETE_PENDING, ERROR_FILE_NOT_FOUND, ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, FILETIME, GetLastError, HANDLE, NTSTATUS,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, STATUS_DELETE_PENDING, STATUS_FILE_DELETED,
    UNICODE_STRING,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileAttributeTagInfo, FileDispositionInfo, FileDispositionInfoEx, FileIdInfo,
    FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetVolumeInformationByHandleW, MoveFileExW, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

mod dir;
mod links;

pub use dir::{DirReader, RawEntry};
pub use links::{LinkQuery, Links};

/// `FILE_SUPPORTS_HARD_LINKS` in the file system flags.
const FILE_SUPPORTS_HARD_LINKS: u32 = 0x0040_0000;

/// An `NTSTATUS` as the Win32 error std maps to an `io::ErrorKind`. A file or directory being
/// deleted is "not found": it was removed during the scan (Win32 would say access denied).
fn nt_error(status: NTSTATUS) -> io::Error {
    let code = match status {
        STATUS_DELETE_PENDING | STATUS_FILE_DELETED => ERROR_FILE_NOT_FOUND,
        // SAFETY: a table lookup on an integer; no pointers.
        s => unsafe { RtlNtStatusToDosError(s) },
    };
    io::Error::from_raw_os_error(code.cast_signed())
}

fn win32_error(code: u32) -> io::Error {
    let code = if code == ERROR_DELETE_PENDING {
        ERROR_FILE_NOT_FOUND
    } else {
        code
    };
    io::Error::from_raw_os_error(code.cast_signed())
}

fn last_error_code() -> u32 {
    // SAFETY: reads this thread's last-error value.
    unsafe { GetLastError() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Whether this process runs elevated (with an administrator's full token), so it may read
/// what administrators may.
#[must_use]
pub fn is_privileged() -> bool {
    let mut h: HANDLE = null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing; `h` is valid
    // for writes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut h) } == 0 {
        return false;
    }
    // SAFETY: on success `h` is a new handle that nothing else owns; `OwnedHandle` closes it.
    let token = unsafe { OwnedHandle::from_raw_handle(h) };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a `TOKEN_ELEVATION` valid for writes of the size passed; `len` is
    // valid for writes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast::<c_void>(),
            len_u32(size_of::<TOKEN_ELEVATION>()),
            &raw mut len,
        )
    };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` exists: `MoveFileExW` without
/// `MOVEFILE_REPLACE_EXISTING` (and without `MOVEFILE_COPY_ALLOWED`, so never a copy).
///
/// # Errors
///
/// `AlreadyExists` when `to` exists, or the error `MoveFileExW` reports.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let z = |p: &Path| -> io::Result<Vec<u16>> {
        let v = verbatim_path(p)?;
        Ok(v.as_os_str().encode_wide().chain([0]).collect())
    };
    let (f, t) = (z(from)?, z(to)?);
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `path` as an absolute verbatim path (`\\?\C:\dir`, `\\?\UNC\server\share\dir`). A relative
/// path is made absolute against the current directory, which also resolves `.` and `..`
/// (`GetFullPathNameW`); a verbatim or device path (`\\?\`, `\\.\`) is returned as it is.
/// Names joined to the result reach the filesystem unchanged.
///
/// # Errors
///
/// When `path` cannot be made absolute (no current directory).
pub fn verbatim_path(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let w: Vec<u16> = abs.as_os_str().encode_wide().collect();
    if w.starts_with(&wide(r"\\?\")) || w.starts_with(&wide(r"\\.\")) {
        return Ok(abs);
    }
    let out = if w.starts_with(&wide(r"\\")) {
        let mut o = wide(r"\\?\UNC\");
        o.extend_from_slice(&w[2..]);
        o
    } else {
        let mut o = wide(r"\\?\");
        o.extend_from_slice(&w);
        o
    };
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&out)))
}

/// The device of the volume `path` is on, as `\\?\Volume{GUID}` (no trailing separator), for
/// reading the volume itself. Opening it needs administrator rights.
///
/// # Errors
///
/// When `path` cannot be made absolute, or the error `GetVolumePathNameW` or
/// `GetVolumeNameForVolumeMountPointW` reports.
pub fn volume_device(path: &Path) -> io::Result<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    let mut p: Vec<u16> = path.as_os_str().encode_wide().collect();
    if p.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    p.push(0);
    let mut mount = vec![0u16; p.len().max(64)];
    // SAFETY: `p` is NUL-terminated; `mount` is valid for writes of its length in characters,
    // which is at least the path's, the longest a mount point of it can be.
    if unsafe { GetVolumePathNameW(p.as_ptr(), mount.as_mut_ptr(), len_u32(mount.len())) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0u16; 64];
    // SAFETY: `mount` is NUL-terminated (the call wrote it); `name` is valid for writes of its
    // length in characters (a volume GUID path takes 50 with its NUL).
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), name.as_mut_ptr(), len_u32(name.len()))
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    if len > 0 && name[len - 1] == u16::from(b'\\') {
        len -= 1;
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&name[..len])))
}

/// `path` in the NT namespace (`\??\C:\dir`), for `NtCreateFile`.
fn nt_path(path: &Path) -> io::Result<Vec<u16>> {
    let v = verbatim_path(path)?;
    let w: Vec<u16> = v.as_os_str().encode_wide().collect();
    if w.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // `\\?\` and `\\.\` both become `\??\`.
    let mut out = wide(r"\??\");
    out.extend_from_slice(&w[4..]);
    Ok(out)
}

/// A counted string pointing into `s`, which must outlive every use of it.
fn unicode(s: &[u16]) -> io::Result<UNICODE_STRING> {
    let bytes = u16::try_from(s.len() * 2)
        .map_err(|_| io::Error::from_raw_os_error(ERROR_FILENAME_EXCED_RANGE.cast_signed()))?;
    Ok(UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        // The callee only reads the string.
        Buffer: s.as_ptr().cast_mut(),
    })
}

fn object_attributes(
    root: Option<BorrowedHandle<'_>>,
    name: &UNICODE_STRING,
    attributes: u32,
) -> OBJECT_ATTRIBUTES {
    OBJECT_ATTRIBUTES {
        Length: len_u32(size_of::<OBJECT_ATTRIBUTES>()),
        RootDirectory: root.map_or(null_mut(), |h| h.as_raw_handle()),
        ObjectName: name,
        Attributes: attributes,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    }
}

/// Opens `name` relative to `root`, or the absolute NT path `name` without one, for
/// synchronous I/O, sharing everything (a scan must not stop others from writing or deleting).
/// `FILE_OPEN_FOR_BACKUP_INTENT` is what `FILE_FLAG_BACKUP_SEMANTICS` sets in `CreateFileW`:
/// it lets directories be opened, and uses the backup privilege if the process has enabled it.
fn nt_open(
    root: Option<BorrowedHandle<'_>>,
    name: &[u16],
    access: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    nt_open_shared(root, name, access, options, share)
}

/// [`nt_open`] with the sharing given.
fn nt_open_shared(
    root: Option<BorrowedHandle<'_>>,
    name: &[u16],
    access: u32,
    options: u32,
    share: u32,
) -> io::Result<OwnedHandle> {
    let us = unicode(name)?;
    // Absolute paths are looked up as `CreateFileW` does; names read from a directory are
    // looked up exactly (case-sensitive directories may hold names differing only in case).
    let attrs = if root.is_some() {
        0
    } else {
        OBJ_CASE_INSENSITIVE
    };
    let oa = object_attributes(root, &us, attrs);
    let mut h: HANDLE = null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `oa` points to `us`, which points into `name`; both outlive the call. `h` and
    // `iosb` are valid for writes. No allocation size or extended attributes are passed, which
    // `FILE_OPEN` allows.
    let st = unsafe {
        NtCreateFile(
            &raw mut h,
            access,
            &raw const oa,
            &raw mut iosb,
            null(),
            0,
            share,
            FILE_OPEN,
            options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            null(),
            0,
        )
    };
    if st < 0 {
        return Err(nt_error(st));
    }
    // SAFETY: on success `h` is a new handle that nothing else owns; `OwnedHandle` closes it
    // exactly once, on every path from here.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// Opens a directory for listing. With `follow`, a symlink or junction at `path` is followed
/// (a scan root given as one); without it the reparse point itself is opened, so a directory
/// replaced by a junction after its parent was listed is not entered.
///
/// # Errors
///
/// The error `NtCreateFile` reports (`NotFound`, `PermissionDenied`, ...).
pub fn open_dir(path: &Path, follow: bool) -> io::Result<OwnedHandle> {
    let reparse = if follow { 0 } else { FILE_OPEN_REPARSE_POINT };
    nt_open(
        None,
        &nt_path(path)?,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE | reparse,
    )
}

/// Structures the OS fills in that consist of integers only, so that every bit pattern is a
/// valid value.
///
/// # Safety
///
/// Implementors must be `repr(C)` with integer (or integer array) fields only.
unsafe trait Plain: Default {}

// SAFETY: u64 and u8 array.
unsafe impl Plain for FILE_ID_INFO {}
// SAFETY: two u32.
unsafe impl Plain for FILE_ATTRIBUTE_TAG_INFO {}
// SAFETY: integers only (see the struct).
unsafe impl Plain for StandardInfo {}
// SAFETY: i64 and u32 fields only.
unsafe impl Plain for FILE_STAT_INFORMATION {}

/// `FILE_STANDARD_INFO` / `FILE_STANDARD_INFORMATION` with its two `BOOLEAN`s as `u8`: the
/// windows-sys versions use `bool`, for which a byte other than 0 or 1 would be undefined.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct StandardInfo {
    allocation_size: i64,
    end_of_file: i64,
    number_of_links: u32,
    delete_pending: u8,
    directory: u8,
}
const _: () = assert!(size_of::<StandardInfo>() == size_of::<FILE_STANDARD_INFORMATION>());
const _: () = assert!(
    offset_of!(StandardInfo, number_of_links)
        == offset_of!(FILE_STANDARD_INFORMATION, NumberOfLinks)
);

/// `GetFileInformationByHandleEx` for a class whose structure is `T`.
fn handle_info<T: Plain>(h: BorrowedHandle<'_>, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut v = T::default();
    // SAFETY: `v` is valid for writes of `size_of::<T>()` bytes, the size passed, and any bytes
    // the OS writes into it form a valid `T` (`Plain`). A class whose structure is larger fails
    // with an error instead of writing past it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h.as_raw_handle(),
            class,
            (&raw mut v).cast::<c_void>(),
            len_u32(size_of::<T>()),
        )
    };
    if ok == 0 {
        Err(win32_error(last_error_code()))
    } else {
        Ok(v)
    }
}

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// Metadata of an open file or directory: attributes, write time, volume serial number and
/// link count from `GetFileInformationByHandle`, sizes from `FileStandardInfo`, the 128-bit
/// file id from `FileIdInfo` (the 64-bit file index where that class is not supported), and
/// the reparse tag from `FileAttributeTagInfo`.
///
/// `alloc` is `AllocationSize`, the clusters of the file's unnamed data stream, as in the
/// directory enumeration. For NTFS-compressed and sparse files that is what the data takes on
/// disk; `GetCompressedFileSizeW` reports the same for them. Files compressed by WOF (`CompactOS`)
/// keep their data in an alternate stream, which neither number includes (unverified).
fn stat_handle(h: BorrowedHandle<'_>) -> io::Result<WinStat> {
    let mut bh = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `bh` is valid for writes; the structure has integer fields only.
    if unsafe { GetFileInformationByHandle(h.as_raw_handle(), &raw mut bh) } == 0 {
        return Err(win32_error(last_error_code()));
    }
    let std: StandardInfo = handle_info(h, FileStandardInfo)?;
    let file_id = match handle_info::<FILE_ID_INFO>(h, FileIdInfo) {
        Ok(i) => u128::from_le_bytes(i.FileId.Identifier),
        Err(_) => (u128::from(bh.nFileIndexHigh) << 32) | u128::from(bh.nFileIndexLow),
    };
    let reparse_tag = if bh.dwFileAttributes & win_attr::REPARSE_POINT != 0 {
        handle_info::<FILE_ATTRIBUTE_TAG_INFO>(h, FileAttributeTagInfo)?.ReparseTag
    } else {
        0
    };
    Ok(WinStat {
        attributes: bh.dwFileAttributes,
        reparse_tag,
        size: std.end_of_file.cast_unsigned(),
        alloc: std.allocation_size.cast_unsigned(),
        last_write: filetime(bh.ftLastWriteTime),
        volume: bh.dwVolumeSerialNumber,
        file_id,
        nlink: bh.nNumberOfLinks,
    })
}

/// [`stat_handle`] of `path`, opened with attribute access only (no data is read, so the
/// file's contents are not touched). With `follow`, a symlink or junction at `path` is
/// followed; without it, the reparse point itself is described.
///
/// # Errors
///
/// The error of opening `path` or of reading its information.
pub fn stat_path(path: &Path, follow: bool) -> io::Result<WinStat> {
    let reparse = if follow { 0 } else { FILE_OPEN_REPARSE_POINT };
    let h = nt_open(
        None,
        &nt_path(path)?,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        reparse,
    )?;
    stat_handle(h.as_handle())
}

/// Opens the entry at `path` itself (a reparse point is not followed) to delete it through the
/// handle ([`delete_open`]). Others may go on reading and writing it, but not delete or rename
/// it while the handle is open, nor rename a directory above it.
///
/// # Errors
///
/// The error `NtCreateFile` reports (`NotFound`, `PermissionDenied`, a sharing violation).
pub fn open_for_delete(path: &Path) -> io::Result<OwnedHandle> {
    nt_open_shared(
        None,
        &nt_path(path)?,
        DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN_REPARSE_POINT,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
}

/// [`stat_path`] of an entry already open.
///
/// # Errors
///
/// The error of reading its information.
pub fn stat_open(h: BorrowedHandle<'_>) -> io::Result<WinStat> {
    stat_handle(h)
}

/// Deletes the entry open as `h` (from [`open_for_delete`]; a directory must be empty): its name
/// goes at once where the filesystem has POSIX semantics (NTFS on Windows 10 1809 and later),
/// otherwise when the handle is closed. A read-only file is deleted too.
///
/// # Errors
///
/// The error `SetFileInformationByHandle` reports (a directory that is not empty, for one).
pub fn delete_open(h: BorrowedHandle<'_>) -> io::Result<()> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the buffer is a `FILE_DISPOSITION_INFO_EX` of the size passed, read only for the
    // duration of the call.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const ex).cast(),
            len_u32(size_of::<FILE_DISPOSITION_INFO_EX>()),
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let code = last_error_code();
    if ![
        ERROR_INVALID_PARAMETER,
        ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED,
    ]
    .contains(&code)
    {
        return Err(win32_error(code));
    }
    // Older systems and other filesystems: the plain disposition, which refuses read-only files.
    let plain = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above, for a `FILE_DISPOSITION_INFO`.
    let ok = unsafe {
        SetFileInformationByHandle(
            h.as_raw_handle(),
            FileDispositionInfo,
            (&raw const plain).cast(),
            len_u32(size_of::<FILE_DISPOSITION_INFO>()),
        )
    };
    if ok == 0 {
        return Err(win32_error(last_error_code()));
    }
    Ok(())
}

/// The volume an open file is on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Volume serial number, the [`WinStat::volume`] of every file on it.
    pub serial: u32,
    /// Files can have several names. False only for filesystems known to have no hard links
    /// (FAT, FAT32, exFAT, CDFS): others may support them without setting
    /// `FILE_SUPPORTS_HARD_LINKS` (Wine does not set it for its NTFS), and missing a link
    /// count would count a file once per name.
    pub hard_links: bool,
}

/// [`Volume`] of an open file or directory (`GetVolumeInformationByHandleW`).
///
/// # Errors
///
/// The error `GetVolumeInformationByHandleW` reports.
pub fn volume_of(h: BorrowedHandle<'_>) -> io::Result<Volume> {
    let (mut serial, mut flags) = (0u32, 0u32);
    let mut fs_name = [0u16; 32];
    // SAFETY: the volume name buffer is null with size 0, which the call allows for a value
    // not wanted; the filesystem name buffer is valid for writes of the length passed (in
    // characters, NUL included); `serial` and `flags` are valid for writes.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            h.as_raw_handle(),
            null_mut(),
            0,
            &raw mut serial,
            null_mut(),
            &raw mut flags,
            fs_name.as_mut_ptr(),
            len_u32(fs_name.len()),
        )
    };
    if ok == 0 {
        return Err(win32_error(last_error_code()));
    }
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    let fs = String::from_utf16_lossy(&fs_name[..len]);
    let no_links = ["FAT", "FAT32", "exFAT", "CDFS"]
        .iter()
        .any(|n| fs.eq_ignore_ascii_case(n));
    Ok(Volume {
        serial,
        hard_links: flags & FILE_SUPPORTS_HARD_LINKS != 0 || !no_links,
    })
}

/// A buffer length as the `u32` Windows calls take. A longer buffer is passed as its first
/// `u32::MAX` bytes or units, never more than it holds.
pub(crate) fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
