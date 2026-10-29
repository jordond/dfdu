//! Directory enumeration with sizes, attributes and file ids (`GetFileInformationByHandleEx`).

use std::ffi::c_void;
use std::io;
use std::mem::offset_of;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};

use dfdu_tree::{WinStat, win_attr};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL, ERROR_INVALID_PARAMETER,
    ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO, FILE_ID_EXTD_DIR_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo,
    GetFileInformationByHandleEx,
};

use super::{last_error_code, len_u32, win32_error};

/// The directory information class an enumeration uses. `FileIdExtdDirectoryInfo` (Windows 8
/// and later) has the 128-bit file ids `ReFS` needs; filesystems without it (some network
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
        // SAFETY: the `u64` slice is non-null and valid for reads of `len * 8` bytes for as long
        // as `self` is borrowed, and nothing writes it meanwhile (`fill` takes `&mut self`);
        // `u8` has no alignment requirement and every bit pattern is a valid `u8`.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.buf.len() * 8) }
    }

    /// The next entry of the directory `dir`, `None` at its end. `.` and `..` are left out.
    /// `dir` must be the same open directory on every call.
    ///
    /// # Errors
    ///
    /// The error `GetFileInformationByHandleEx` reports, or `InvalidData` for a malformed entry.
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
                get(at, 4)?
                    .try_into()
                    .map(u32::from_le_bytes)
                    .map_err(|_| corrupt())
            };
            let u64_at = |at: usize| -> io::Result<u64> {
                get(at, 8)?
                    .try_into()
                    .map(u64::from_le_bytes)
                    .map_err(|_| corrupt())
            };
            let next = u32_at(0)? as usize;
            let name_len = u32_at(l.name_len)? as usize;
            let name = off + l.name..off + l.name + name_len;
            if !name_len.is_multiple_of(2) || b.get(name.clone()).is_none() {
                return Err(corrupt());
            }
            let attributes = u32_at(l.attributes)?;
            let file_id = match l.id {
                (at, 16) => get(at, 16)?
                    .try_into()
                    .map(u128::from_le_bytes)
                    .map_err(|_| corrupt())?,
                (at, 8) => u128::from(u64_at(at)?),
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
            let len = len_u32(self.buf.len() * 8);
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
                code @ (ERROR_INVALID_PARAMETER
                | ERROR_INVALID_LEVEL
                | ERROR_NOT_SUPPORTED
                | ERROR_INVALID_FUNCTION)
                    if !self.started =>
                {
                    let Some(class) = self.class.fallback() else {
                        return Err(win32_error(code));
                    };
                    self.class = class;
                    self.restart = true;
                }
                code => return Err(win32_error(code)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
