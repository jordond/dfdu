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
use std::sync::atomic::{AtomicBool, Ordering};

use dfdu_tree::{WinStat, win_attr};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT,
    FILE_STANDARD_INFORMATION, FILE_STAT_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
    FileStandardInformation, FileStatInformation, NtCreateFile, NtQueryInformationByName,
    NtQueryInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_DELETE_PENDING, ERROR_FILE_NOT_FOUND, ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_LEVEL, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
    FILETIME, GetLastError, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
    STATUS_DELETE_PENDING, STATUS_FILE_DELETED, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER, STATUS_NOT_IMPLEMENTED,
    STATUS_NOT_SUPPORTED, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TAG_INFO, FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO,
    FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileAttributeTagInfo, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdExtdDirectoryInfo,
    FileIdExtdDirectoryRestartInfo, FileIdInfo, FileStandardInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetVolumeInformationByHandleW, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

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
    io::Error::from_raw_os_error(code as i32)
}

fn win32_error(code: u32) -> io::Error {
    let code = if code == ERROR_DELETE_PENDING {
        ERROR_FILE_NOT_FOUND
    } else {
        code
    };
    io::Error::from_raw_os_error(code as i32)
}

fn last_error_code() -> u32 {
    // SAFETY: reads this thread's last-error value.
    unsafe { GetLastError() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `path` as an absolute verbatim path (`\\?\C:\dir`, `\\?\UNC\server\share\dir`). A relative
/// path is made absolute against the current directory, which also resolves `.` and `..`
/// (`GetFullPathNameW`); a verbatim or device path (`\\?\`, `\\.\`) is returned as it is.
/// Names joined to the result reach the filesystem unchanged.
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
        .map_err(|_| io::Error::from_raw_os_error(ERROR_FILENAME_EXCED_RANGE as i32))?;
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
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
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
            &mut h,
            access,
            &oa,
            &mut iosb,
            null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
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
            size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        Err(win32_error(last_error_code()))
    } else {
        Ok(v)
    }
}

fn filetime(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// Metadata of an open file or directory: attributes, write time, volume serial number and
/// link count from `GetFileInformationByHandle`, sizes from `FileStandardInfo`, the 128-bit
/// file id from `FileIdInfo` (the 64-bit file index where that class is not supported), and
/// the reparse tag from `FileAttributeTagInfo`.
///
/// `alloc` is `AllocationSize`, the clusters of the file's unnamed data stream, as in the
/// directory enumeration. For NTFS-compressed and sparse files that is what the data takes on
/// disk; `GetCompressedFileSizeW` reports the same for them. Files compressed by WOF (CompactOS)
/// keep their data in an alternate stream, which neither number includes (unverified).
fn stat_handle(h: BorrowedHandle<'_>) -> io::Result<WinStat> {
    let mut bh = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `bh` is valid for writes; the structure has integer fields only.
    if unsafe { GetFileInformationByHandle(h.as_raw_handle(), &mut bh) } == 0 {
        return Err(win32_error(last_error_code()));
    }
    let std: StandardInfo = handle_info(h, FileStandardInfo)?;
    let file_id = match handle_info::<FILE_ID_INFO>(h, FileIdInfo) {
        Ok(i) => u128::from_le_bytes(i.FileId.Identifier),
        Err(_) => ((bh.nFileIndexHigh as u128) << 32) | bh.nFileIndexLow as u128,
    };
    let reparse_tag = if bh.dwFileAttributes & win_attr::REPARSE_POINT != 0 {
        handle_info::<FILE_ATTRIBUTE_TAG_INFO>(h, FileAttributeTagInfo)?.ReparseTag
    } else {
        0
    };
    Ok(WinStat {
        attributes: bh.dwFileAttributes,
        reparse_tag,
        size: std.end_of_file as u64,
        alloc: std.allocation_size as u64,
        last_write: filetime(bh.ftLastWriteTime),
        volume: bh.dwVolumeSerialNumber,
        file_id,
        nlink: bh.nNumberOfLinks,
    })
}

/// [`stat_handle`] of `path`, opened with attribute access only (no data is read, so the
/// file's contents are not touched). With `follow`, a symlink or junction at `path` is
/// followed; without it, the reparse point itself is described.
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
            &mut serial,
            null_mut(),
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
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

/// The directory information class an enumeration uses. `FileIdExtdDirectoryInfo` (Windows 8
/// and later) has the 128-bit file ids ReFS needs; filesystems without it (some network
/// redirectors and third-party drivers) fall back to `FileIdBothDirectoryInfo` (64-bit ids),
/// then to `FileFullDirectoryInfo` (no ids).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Class {
    IdExtd,
    IdBoth,
    Full,
}

/// Where the fields of one entry are, for a [`Class`].
struct Layout {
    last_write: usize,
    size: usize,
    alloc: usize,
    attributes: usize,
    name_len: usize,
    /// The reparse tag: its own field in `FILE_ID_EXTD_DIR_INFO`; in the others `EaSize`
    /// holds it when the entry is a reparse point.
    tag: usize,
    /// The file id and its length in bytes (0: none).
    id: (usize, usize),
    name: usize,
}

macro_rules! layout {
    ($t:ty, $tag:ident, $id:expr) => {
        Layout {
            last_write: offset_of!($t, LastWriteTime),
            size: offset_of!($t, EndOfFile),
            alloc: offset_of!($t, AllocationSize),
            attributes: offset_of!($t, FileAttributes),
            name_len: offset_of!($t, FileNameLength),
            tag: offset_of!($t, $tag),
            id: $id,
            name: offset_of!($t, FileName),
        }
    };
}

const EXTD: Layout = layout!(
    FILE_ID_EXTD_DIR_INFO,
    ReparsePointTag,
    (offset_of!(FILE_ID_EXTD_DIR_INFO, FileId), 16)
);
const BOTH: Layout = layout!(
    FILE_ID_BOTH_DIR_INFO,
    EaSize,
    (offset_of!(FILE_ID_BOTH_DIR_INFO, FileId), 8)
);
const FULL: Layout = layout!(FILE_FULL_DIR_INFO, EaSize, (0, 0));
// `NextEntryOffset` comes first in all three.
const _: () = assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, NextEntryOffset) == 0);
const _: () = assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset) == 0);
const _: () = assert!(offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset) == 0);

impl Class {
    fn info_class(self, restart: bool) -> FILE_INFO_BY_HANDLE_CLASS {
        match (self, restart) {
            (Class::IdExtd, false) => FileIdExtdDirectoryInfo,
            (Class::IdExtd, true) => FileIdExtdDirectoryRestartInfo,
            (Class::IdBoth, false) => FileIdBothDirectoryInfo,
            (Class::IdBoth, true) => FileIdBothDirectoryRestartInfo,
            (Class::Full, false) => FileFullDirectoryInfo,
            (Class::Full, true) => FileFullDirectoryRestartInfo,
        }
    }

    fn fallback(self) -> Option<Class> {
        match self {
            Class::IdExtd => Some(Class::IdBoth),
            Class::IdBoth => Some(Class::Full),
            Class::Full => None,
        }
    }

    fn layout(self) -> &'static Layout {
        match self {
            Class::IdExtd => &EXTD,
            Class::IdBoth => &BOTH,
            Class::Full => &FULL,
        }
    }
}

/// The enumeration buffer, in 8-byte words: the directory information structures must be
/// 8-byte aligned. 64 KiB holds about 600 entries with 20-character names.
const DIR_BUF_WORDS: usize = 8 * 1024;

/// Reads a directory's entries with their sizes, attributes and file ids, a buffer at a time
/// (`GetFileInformationByHandleEx`), without opening any of them.
pub struct DirReader {
    buf: Box<[u64]>,
    class: Class,
    /// The next call restarts the enumeration (after switching to a fallback class).
    restart: bool,
    /// Offset of the next entry in the buffer; `None` when the buffer is used up.
    next: Option<usize>,
    /// A call has returned entries, so the class works on this directory.
    started: bool,
    done: bool,
}

pub struct RawEntry<'a> {
    /// The name in UTF-16, as little-endian bytes ([`RawEntry::name_units`]).
    pub name: &'a [u8],
    /// Everything but `volume` (0) and `nlink` (1), which the enumeration does not report.
    /// `file_id` is 0 when the filesystem supports neither id class.
    pub stat: WinStat,
}

impl RawEntry<'_> {
    pub fn name_units(&self) -> impl Iterator<Item = u16> + '_ {
        self.name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
    }
}

fn corrupt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed directory information",
    )
}

impl Default for DirReader {
    fn default() -> Self {
        DirReader {
            buf: vec![0u64; DIR_BUF_WORDS].into_boxed_slice(),
            class: Class::IdExtd,
            restart: false,
            next: None,
            started: false,
            done: false,
        }
    }
}

impl DirReader {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the `u64` slice is valid for reads of `len * 8` bytes for as long as `self`
        // is borrowed; `u8` has no alignment requirement and every bit pattern is a valid `u8`.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.buf.len() * 8) }
    }

    /// The next entry of the directory `dir`, `None` at its end. `.` and `..` are left out.
    /// `dir` must be the same open directory on every call.
    pub fn next(&mut self, dir: BorrowedHandle<'_>) -> io::Result<Option<RawEntry<'_>>> {
        loop {
            let Some(off) = self.next else {
                if self.done {
                    return Ok(None);
                }
                self.fill(dir)?;
                continue;
            };
            let l = self.class.layout();
            let b = self.bytes();
            let get = |at: usize, n: usize| b.get(off + at..off + at + n).ok_or_else(corrupt);
            let u32_at = |at: usize| -> io::Result<u32> {
                Ok(u32::from_le_bytes(get(at, 4)?.try_into().expect("4 bytes")))
            };
            let u64_at = |at: usize| -> io::Result<u64> {
                Ok(u64::from_le_bytes(get(at, 8)?.try_into().expect("8 bytes")))
            };
            let next = u32_at(0)? as usize;
            let name_len = u32_at(l.name_len)? as usize;
            let name = off + l.name..off + l.name + name_len;
            if !name_len.is_multiple_of(2) || b.get(name.clone()).is_none() {
                return Err(corrupt());
            }
            let attributes = u32_at(l.attributes)?;
            let file_id = match l.id {
                (at, 16) => u128::from_le_bytes(get(at, 16)?.try_into().expect("16 bytes")),
                (at, 8) => u64_at(at)? as u128,
                _ => 0,
            };
            let stat = WinStat {
                attributes,
                reparse_tag: if attributes & win_attr::REPARSE_POINT != 0 {
                    u32_at(l.tag)?
                } else {
                    0
                },
                size: u64_at(l.size)?,
                alloc: u64_at(l.alloc)?,
                last_write: u64_at(l.last_write)?,
                volume: 0,
                file_id,
                nlink: 1,
            };
            let dots = matches!(&b[name.clone()], [b'.', 0] | [b'.', 0, b'.', 0]);
            // The chain ends with offset 0; every field is read with bounds checks, so a
            // misaligned entry is read correctly too.
            let following = match next {
                0 => None,
                n if off + n < b.len() => Some(off + n),
                _ => return Err(corrupt()),
            };
            self.next = following;
            if dots {
                continue;
            }
            return Ok(Some(RawEntry {
                name: &self.bytes()[name],
                stat,
            }));
        }
    }

    fn fill(&mut self, dir: BorrowedHandle<'_>) -> io::Result<()> {
        loop {
            // A call that succeeds without writing would otherwise leave the previous buffer's
            // first entry in place; an empty name ends the directory instead.
            self.buf[..12].fill(0);
            let len = (self.buf.len() * 8) as u32;
            // SAFETY: the buffer is valid for writes of `len` bytes and 8-byte aligned, as the
            // directory information classes require; the call writes at most `len` bytes, and
            // every bit pattern is a valid `u64`.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    dir.as_raw_handle(),
                    self.class.info_class(self.restart),
                    self.buf.as_mut_ptr().cast::<c_void>(),
                    len,
                )
            };
            if ok != 0 {
                self.restart = false;
                self.started = true;
                let name_len = self.class.layout().name_len;
                let first = &self.bytes()[name_len..name_len + 4];
                if first == [0; 4] {
                    self.done = true;
                    self.next = None;
                } else {
                    self.next = Some(0);
                }
                return Ok(());
            }
            match last_error_code() {
                // The end, or an empty directory without `.` and `..` (a volume root on FAT).
                ERROR_NO_MORE_FILES | ERROR_FILE_NOT_FOUND => {
                    self.done = true;
                    return Ok(());
                }
                ERROR_INVALID_PARAMETER
                | ERROR_INVALID_LEVEL
                | ERROR_NOT_SUPPORTED
                | ERROR_INVALID_FUNCTION
                    if !self.started && self.class.fallback().is_some() =>
                {
                    self.class = self.class.fallback().expect("checked");
                    self.restart = true;
                }
                code => return Err(win32_error(code)),
            }
        }
    }
}

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
    /// are opened with `FILE_OPEN_REPARSE_POINT`, as [`stat_path`] does.
    pub fn query(&self, dir: BorrowedHandle<'_>, name: &[u16], reparse: bool) -> io::Result<Links> {
        if name.is_empty() || name.contains(&(b'\\' as u16)) || name.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if !reparse && self.by_name.load(Ordering::Relaxed) {
            match by_name(dir, name)? {
                Ok(l) => return Ok(l),
                Err(()) => self.by_name.store(false, Ordering::Relaxed),
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
                &mut iosb,
                (&raw mut info).cast::<c_void>(),
                size_of::<StandardInfo>() as u32,
                FileStandardInformation,
            )
        };
        if st < 0 {
            return Err(nt_error(st));
        }
        Ok(Links {
            nlink: info.number_of_links,
            size: info.end_of_file as u64,
            alloc: info.allocation_size as u64,
        })
    }
}

/// `NtQueryInformationByName`; `Ok(Err(()))` when the call or class is not supported.
fn by_name(dir: BorrowedHandle<'_>, name: &[u16]) -> io::Result<Result<Links, ()>> {
    let us = unicode(name)?;
    let oa = object_attributes(Some(dir), &us, 0);
    let mut iosb = IO_STATUS_BLOCK::default();
    let mut info = FILE_STAT_INFORMATION::default();
    // SAFETY: `oa` points to `us`, which points into `name`; both outlive the call. `info` is
    // valid for writes of the size passed and any bytes written form a valid value (`Plain`);
    // `iosb` is valid for writes.
    let st = unsafe {
        NtQueryInformationByName(
            &oa,
            &mut iosb,
            (&raw mut info).cast::<c_void>(),
            size_of::<FILE_STAT_INFORMATION>() as u32,
            FileStatInformation,
        )
    };
    match st {
        s if s >= 0 => Ok(Ok(Links {
            nlink: info.NumberOfLinks,
            size: info.EndOfFile as u64,
            alloc: info.AllocationSize as u64,
        })),
        STATUS_INVALID_INFO_CLASS
        | STATUS_NOT_IMPLEMENTED
        | STATUS_NOT_SUPPORTED
        | STATUS_INVALID_PARAMETER
        | STATUS_INVALID_DEVICE_REQUEST => Ok(Err(())),
        s => Err(nt_error(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_paths() {
        let v = |p: &str| verbatim_path(Path::new(p)).unwrap();
        assert_eq!(v(r"C:\a\..\b"), Path::new(r"\\?\C:\b"));
        assert_eq!(v(r"\\server\share\x"), Path::new(r"\\?\UNC\server\share\x"));
        assert_eq!(v(r"\\?\C:\a\..\b."), Path::new(r"\\?\C:\a\..\b."));
        assert_eq!(nt_path(Path::new(r"C:\x")).unwrap(), wide(r"\??\C:\x"));
    }

    /// Enumeration, link counts and stat agree on a small tree.
    #[test]
    fn reads_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("f"), vec![1u8; 5000]).unwrap();
        std::fs::hard_link(root.join("f"), root.join("sub").join("g")).unwrap();
        let dir = open_dir(root, false).unwrap();
        let vol = volume_of(dir.as_handle()).unwrap();
        let mut r = DirReader::default();
        let mut names = Vec::new();
        while let Some(e) = r.next(dir.as_handle()).unwrap() {
            let name: Vec<u16> = e.name_units().collect();
            names.push((String::from_utf16(&name).unwrap(), e.stat));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(names.len(), 2, "{names:?}");
        let (f, sub) = (&names[0].1, &names[1].1);
        assert_eq!(f.size, 5000);
        let fs = stat_path(&root.join("f"), false).unwrap();
        assert_eq!((fs.volume, fs.nlink, fs.size), (vol.serial, 2, 5000));
        assert_eq!(fs.file_id, f.file_id);
        assert_eq!(fs.alloc, f.alloc);
        assert_eq!(sub.attributes & win_attr::DIRECTORY, win_attr::DIRECTORY);
        if vol.hard_links {
            let q = LinkQuery::default();
            let l = q.query(dir.as_handle(), &wide("f"), false).unwrap();
            assert_eq!((l.nlink, l.size, l.alloc), (2, 5000, fs.alloc));
            let e = q
                .query(dir.as_handle(), &wide("missing"), false)
                .unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
        }
        let e = open_dir(&root.join("missing"), false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}
