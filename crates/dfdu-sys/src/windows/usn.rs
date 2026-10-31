//! The NTFS (and `ReFS`) change journal: where it stands, and which directories below a root
//! changed since a position, for rescanning only those.
//!
//! Both calls work unprivileged on Windows 10 1709 and later: the journal is queried and read
//! through a handle to the scan root (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`, which leaves the
//! file names out; they are not needed). A changed directory is the parent named by each
//! record, resolved to a path by its file id (`OpenFileById`, `GetFinalPathNameByHandleW`).
//! A directory that cannot be opened by id any more was deleted or moved, and its old parent
//! has a record of that.

// Every call here is a Windows API call on buffers this module owns.
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdType, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    OpenFileById,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0,
    USN_JOURNAL_DATA_V0,
};

/// Marks a journal identity as a USN journal's (the rest is the journal id and the volume
/// serial number).
const TAG: [u8; 4] = *b"USN\0";

/// More directories than this changed on the volume: rescanning everything is as quick.
const MAX_DIRS: usize = 200_000;

/// What changed below a root since a position ([`usn_changes_since`]).
#[derive(Debug, Default)]
pub struct UsnChanges {
    /// Changed directories relative to the root (the root itself is the empty path).
    pub dirs: Vec<PathBuf>,
    /// The journal cannot tell: it was deleted, recreated or wrapped past the position, or
    /// too much changed.
    pub full: bool,
}

fn open(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn serial(f: &File) -> io::Result<u32> {
    let mut serial = 0u32;
    // SAFETY: every buffer not wanted is null with size 0, which the call allows; `serial` is
    // valid for writes.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            f.as_raw_handle(),
            null_mut(),
            0,
            &raw mut serial,
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(serial)
}

fn query(f: &File) -> io::Result<USN_JOURNAL_DATA_V0> {
    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut got = 0u32;
    // SAFETY: no input buffer; the output buffer is `data`, valid for writes of its size;
    // `got` is valid for writes; no overlapped I/O.
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            FSCTL_QUERY_USN_JOURNAL,
            null(),
            0,
            (&raw mut data).cast::<c_void>(),
            super::len_u32(size_of::<USN_JOURNAL_DATA_V0>()),
            &raw mut got,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(data)
}

fn identity(journal: u64, serial: u32) -> [u8; 16] {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&journal.to_le_bytes());
    id[8..12].copy_from_slice(&serial.to_le_bytes());
    id[12..].copy_from_slice(&TAG);
    id
}

/// The journal of `path`'s volume, as a 16-byte identity, and its next position. Fails where
/// the volume keeps no journal (FAT, network shares, or a journal never created).
///
/// # Errors
///
/// When `path`'s volume cannot be opened or keeps no journal.
pub fn usn_position(path: &Path) -> io::Result<([u8; 16], u64)> {
    let f = open(path)?;
    let data = query(&f)?;
    Ok((
        identity(data.UsnJournalID, serial(&f)?),
        data.NextUsn.cast_unsigned(),
    ))
}

/// A record's parent as an `OpenFileById` descriptor key: 64-bit ids (version 2 records) or
/// 128-bit ones (version 3, `ReFS`).
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Parent {
    Short(u64),
    Long([u8; 16]),
}

/// The directories below `root` that changed after position `usn` of journal `id`
/// ([`usn_position`]), relative to `root`.
///
/// # Errors
///
/// When `root`'s volume cannot be opened or its journal read, or a record is malformed.
pub fn usn_changes_since(root: &Path, id: [u8; 16], usn: u64) -> io::Result<UsnChanges> {
    let full = || {
        Ok(UsnChanges {
            dirs: vec![],
            full: true,
        })
    };
    let f = open(root)?;
    let data = query(&f)?;
    if identity(data.UsnJournalID, serial(&f)?) != id
        || usn.cast_signed() < data.FirstUsn
        || usn.cast_signed() < data.LowestValidUsn
    {
        return full();
    }
    let end = data.NextUsn;
    let mut parents: HashSet<Parent> = HashSet::new();
    let mut next = usn.cast_signed();
    // 8-byte aligned: records hold 64-bit fields.
    let mut buf = vec![0u64; 64 * 1024 / 8];
    while next < end {
        let mut req = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: next,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: data.UsnJournalID,
        };
        let mut got = 0u32;
        // SAFETY: the input is `req`, valid for reads of its size; the output buffer is
        // `buf`, valid for writes of its length in bytes; `got` is valid for writes; no
        // overlapped I/O.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                FSCTL_READ_UNPRIVILEGED_USN_JOURNAL,
                (&raw mut req).cast::<c_void>(),
                super::len_u32(size_of::<READ_USN_JOURNAL_DATA_V0>()),
                buf.as_mut_ptr().cast::<c_void>(),
                super::len_u32(buf.len() * 8),
                &raw mut got,
                null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: reads this thread's last-error value.
            let code = unsafe { GetLastError() };
            // ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_NOT_ACTIVE,
            // ERROR_JOURNAL_ENTRY_DELETED: the history is gone.
            if matches!(code, 1178 | 1179 | 1181) {
                return full();
            }
            return Err(io::Error::from_raw_os_error(code.cast_signed()));
        }
        let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
        let (n, more) = parse(&bytes[..(got as usize).min(bytes.len())], &mut parents)?;
        if parents.len() > MAX_DIRS {
            return full();
        }
        if n <= next || !more {
            break;
        }
        next = n;
    }
    let base = final_path(&f)?;
    let mut dirs = Vec::new();
    for p in parents {
        let Ok(path) = path_of(&f, p) else {
            continue;
        };
        if let Ok(rel) = path.strip_prefix(&base) {
            dirs.push(rel.to_path_buf());
        }
    }
    dirs.sort();
    dirs.dedup();
    Ok(UsnChanges { dirs, full: false })
}

/// Reads one buffer of `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL` output: the next position, then
/// records. Adds each record's parent to `parents`; returns the next position and whether
/// there were records.
fn parse(out: &[u8], parents: &mut HashSet<Parent>) -> io::Result<(i64, bool)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed USN journal record");
    let u16_at = |b: &[u8], at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    let u64_at = |b: &[u8], at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
    };
    let next = u64_at(out, 0).ok_or_else(bad)?.cast_signed();
    let mut at = 8;
    let mut any = false;
    while at < out.len() {
        let rec = &out[at..];
        let len = u32_at(rec, 0).ok_or_else(bad)? as usize;
        if len < 8 || len > rec.len() {
            return Err(bad());
        }
        let rec = &rec[..len];
        any = true;
        // USN_RECORD_V2: the parent's 64-bit id at 16; USN_RECORD_V3: 128-bit ids, the
        // parent's at 24. Version 4 records (ranges) name no parent.
        match u16_at(rec, 4).ok_or_else(bad)? {
            2 => {
                parents.insert(Parent::Short(u64_at(rec, 16).ok_or_else(bad)?));
            }
            3 => {
                let id: [u8; 16] = rec
                    .get(24..40)
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(bad)?;
                parents.insert(Parent::Long(id));
            }
            _ => {}
        }
        at += len;
    }
    Ok((next, any))
}

/// The path of the directory with id `p` on `volume`'s volume.
fn path_of(volume: &File, p: Parent) -> io::Result<PathBuf> {
    let mut desc = FILE_ID_DESCRIPTOR {
        dwSize: super::len_u32(size_of::<FILE_ID_DESCRIPTOR>()),
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: 0 },
    };
    match p {
        Parent::Short(id) => desc.Anonymous.FileId = id.cast_signed(),
        Parent::Long(id) => {
            desc.Type = ExtendedFileIdType;
            desc.Anonymous.ExtendedFileId.Identifier = id;
        }
    }
    // SAFETY: `desc` is a valid descriptor of the size it states; no security attributes;
    // the volume hint is an open handle.
    let h = unsafe {
        OpenFileById(
            volume.as_raw_handle(),
            &raw const desc,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a handle just opened and owned by nothing else; it is closed exactly
    // once, when the `File` drops.
    let f = File::from(unsafe { OwnedHandle::from_raw_handle(h) });
    final_path(&f)
}

/// The normalized path of an open file, in the `\\?\C:\...` form.
fn final_path(f: &File) -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `buf` is valid for writes of its length in characters.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                f.as_raw_handle(),
                buf.as_mut_ptr(),
                super::len_u32(buf.len()),
                FILE_NAME_NORMALIZED,
            )
        } as usize;
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
        }
        // Too small: `n` is the length needed, NUL included.
        buf.resize(n, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records are walked by their lengths; a length past the buffer is an error, not a panic.
    #[test]
    fn records_are_parsed() {
        let mut out = 99u64.to_le_bytes().to_vec();
        let mut v2 = vec![0u8; 64];
        v2[..4].copy_from_slice(&64u32.to_le_bytes());
        v2[4..6].copy_from_slice(&2u16.to_le_bytes());
        v2[16..24].copy_from_slice(&7u64.to_le_bytes());
        let mut v3 = vec![0u8; 80];
        v3[..4].copy_from_slice(&80u32.to_le_bytes());
        v3[4..6].copy_from_slice(&3u16.to_le_bytes());
        v3[24..40].copy_from_slice(&[5; 16]);
        out.extend(&v2);
        out.extend(&v3);
        let mut parents = HashSet::new();
        assert_eq!(parse(&out, &mut parents).unwrap(), (99, true));
        assert!(parents.contains(&Parent::Short(7)));
        assert!(parents.contains(&Parent::Long([5; 16])));
        assert_eq!(
            parse(&99u64.to_le_bytes(), &mut parents).unwrap(),
            (99, false)
        );
        out[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(parse(&out, &mut parents).is_err());
    }

    /// A directory changed after the position is found, relative to the root; nothing is
    /// found for a journal other than the root's.
    #[test]
    fn changes_come_back_relative_to_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir_all(root.join("a\\b")).unwrap();
        std::fs::create_dir_all(root.join("quiet")).unwrap();
        let Ok((id, usn)) = usn_position(&root) else {
            eprintln!("skipped: no change journal");
            return;
        };
        std::fs::write(root.join("a\\b\\new"), b"x").unwrap();
        let c = usn_changes_since(&root, id, usn).unwrap();
        assert!(!c.full);
        assert!(c.dirs.contains(&PathBuf::from("a\\b")), "{:?}", c.dirs);
        assert!(
            !c.dirs.iter().any(|d| d.starts_with("quiet")),
            "{:?}",
            c.dirs
        );
        let mut other = id;
        other[0] ^= 0xff;
        assert!(usn_changes_since(&root, other, usn).unwrap().full);
    }
}
