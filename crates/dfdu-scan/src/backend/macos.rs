//! macOS backend: `getattrlistbulk` reads a directory's entries with their
//! metadata, about a thousand per call into a 128 KiB buffer, so files need no `lstat`.
//!
//! The split between reading and stat'ing falls differently than on Linux:
//! [`DirStream::next_raw`] makes the `getattrlistbulk` calls and turns each file's record into
//! its [`Meta`] there. [`RawBlock::stat`], which may run on another worker, calls `fstatat`
//! relative to the directory only for the entries the records cannot describe exactly:
//! - directories: their records carry no `st_nlink` (`ATTR_DIR_LINKCOUNT` is a different
//!   number), and a mount point's record describes the directory it covers, where `lstat`
//!   describes the root of the mounted volume, which is what mount decisions need;
//! - records with `ATTR_CMN_ERROR`, or without one of the attributes asked for
//!   (`ATTR_CMN_RETURNED_ATTRS` says which came back).
//!
//! Sizes are those `lstat` reports: `ATTR_FILE_DATALENGTH` is `st_size`, and
//! `ATTR_FILE_ALLOCSIZE` counts all forks as `st_blocks` does (decmpfs-compressed files keep
//! their data in the resource fork), rounded up to 512-byte blocks like `st_blocks`.
//!
//! Firmlinks (`SF_FIRMLINK`, macOS 10.15) join directories of the read-only system volume to
//! the Data volume (`/Users` to `/System/Volumes/Data/Users`), so a scan of `/` would see the
//! Data volume twice. They are reported as symlinks of size 0 with the record's own device and
//! inode: shown, not descended. The data is counted once, under `/System/Volumes/Data`, which
//! is a separate APFS volume and so a mount (`st_dev` differs): scanned with `--cross-mounts`,
//! or by scanning it or a directory below a firmlink (`/Users/me`) directly.
//!
//! Elsewhere a wide directory's subdirectories can be read by other workers while its own
//! enumeration continues. On SMB (`smbfs`), where the kernel restarts an enumeration that
//! another one interrupted and a scan can then loop, every directory is read to the end in its
//! first [`DirStream::next_raw`] call, before any subdirectory task exists, and handed out in
//! blocks from memory.
//!
//! Directories are opened by full path with `O_NOFOLLOW`, in pieces when the path is longer
//! than `PATH_MAX` (1024 bytes). Permission errors, including directories that TCC protects
//! without Full Disk Access (`EPERM`), are recorded as permission denied.

use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, OsStr};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_sys::{BulkEntry, SF_FIRMLINK, VDIR, VLNK, VREG};
use dfdu_tree::{ErrKind, Kind, Meta};
use parking_lot::Mutex;
use rustix::fs::{AtFlags, FileType, OFlags};

use super::unix::{UnixStat, name_at, push_name, unix_path_queries};
use super::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock, portable_queries};

/// `getattrlistbulk` buffer; Apple suggests 64-128 KiB.
const BULK_BUF: usize = 128 * 1024;
/// Buffers kept for reuse; more are freed (only drained SMB directories need more at once).
const POOL_MAX: usize = 32;

#[derive(Default, Debug)]
struct Common {
    /// Free `getattrlistbulk` buffers: a 128 KiB allocation per directory would cost more than
    /// reading a small directory.
    pool: Mutex<Vec<Vec<u8>>>,
    /// Devices whose directories are drained in the first read (`smbfs`), by `st_dev`.
    drain: Mutex<HashMap<i32, bool>>,
}

impl Common {
    fn take(&self) -> Vec<u8> {
        self.pool.lock().pop().unwrap_or_else(|| vec![0; BULK_BUF])
    }

    fn give(&self, buf: Vec<u8>) {
        let mut p = self.pool.lock();
        if p.len() < POOL_MAX {
            p.push(buf);
        }
    }

    /// Whether directories on device `dev`, of which `dir` is one, must be drained.
    fn drains(&self, dev: i32, dir: BorrowedFd<'_>) -> bool {
        if let Some(&d) = self.drain.lock().get(&dev) {
            return d;
        }
        let d = rustix::fs::fstatfs(dir).is_ok_and(|st| {
            let name: Vec<u8> = st
                .f_fstypename
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c.cast_unsigned())
                .collect();
            name == b"smbfs"
        });
        self.drain.lock().insert(dev, d);
        d
    }
}

#[derive(Default, Debug)]
pub struct MacBackend {
    portable: PortableBackend,
    common: Arc<Common>,
}

/// One `getattrlistbulk` result not yet parsed to the end.
struct Filled {
    buf: Vec<u8>,
    /// Byte offset of the next record.
    pos: usize,
    /// Records not yet parsed.
    left: u32,
}

pub struct MacStream {
    /// Shared with the raw blocks, which stat relative to it, possibly on other workers.
    fd: Arc<OwnedFd>,
    path: Arc<PathBuf>,
    common: Arc<Common>,
    filled: VecDeque<Filled>,
    /// Whether the first read decided if the directory is drained.
    checked: bool,
    drain: bool,
    /// The read failed; returned once the entries before it are out.
    err: Option<io::Error>,
    /// No more `getattrlistbulk` calls (the end, or an error).
    done: bool,
}

impl Drop for MacStream {
    fn drop(&mut self) {
        for f in self.filled.drain(..) {
            self.common.give(f.buf);
        }
    }
}

/// What a block entry needs before it can be reported.
#[derive(Copy, Clone, Debug)]
enum Pending {
    /// Complete from its record.
    Ready(Meta),
    /// `fstatat` it.
    Stat,
}

/// Entries read by `getattrlistbulk`: names NUL-terminated in one buffer, with what each
/// still needs.
struct MacRaw {
    fd: Arc<OwnedFd>,
    path: Arc<PathBuf>,
    names: Vec<u8>,
    /// Start of each name in `names` (it ends at its NUL).
    entries: Vec<(u32, Pending)>,
}

impl MacRaw {
    fn add(&mut self, e: &BulkEntry<'_>) {
        if e.name == b"." || e.name == b".." {
            return;
        }
        let off = push_name(&mut self.names, e.name);
        self.entries.push((off, pending(e)));
    }
}

/// [`Pending`] for one record: files described in full are ready, firmlinks become links,
/// everything else is stat'ed.
fn pending(e: &BulkEntry<'_>) -> Pending {
    let meta = |kind, apparent, alloc, nlink, (dev, ino, mtime): (i32, u64, i64)| Meta {
        kind,
        apparent,
        alloc,
        // As std reports it: times before the epoch are 0.
        mtime: mtime.max(0),
        // `dev_t` is an `i32`; std widens `st_dev` with sign extension, and so does this.
        dev: i64::from(dev).cast_unsigned(),
        ino,
        nlink,
        mount: 0,
    };
    // Never stat'ed: `fstatat` would go through the firmlink and report the directory it
    // leads to, which is counted on the Data volume.
    if e.obj_type == Some(VDIR) && e.flags.is_some_and(|f| f & SF_FIRMLINK != 0) {
        let id = (
            e.dev.unwrap_or(0),
            e.ino.unwrap_or(0),
            e.mtime.map_or(0, |t| t.0),
        );
        return Pending::Ready(meta(Kind::Symlink, 0, 0, 1, id));
    }
    if e.error != 0 {
        return Pending::Stat;
    }
    let (Some(t), Some(dev), Some(ino), Some((mtime, _))) = (e.obj_type, e.dev, e.ino, e.mtime)
    else {
        return Pending::Stat;
    };
    if t == VDIR {
        return Pending::Stat;
    }
    let (Some(nlink), Some(alloc), Some(size)) = (e.nlink, e.alloc, e.size) else {
        return Pending::Stat;
    };
    let kind = match t {
        VREG => Kind::File,
        VLNK => Kind::Symlink,
        _ => Kind::Other,
    };
    // `st_blocks` is the allocation rounded up to 512-byte blocks.
    let alloc = u64::try_from(alloc).unwrap_or(0).div_ceil(512) * 512;
    Pending::Ready(meta(
        kind,
        size.cast_unsigned(),
        alloc,
        nlink,
        (dev, ino, mtime),
    ))
}

impl RawBlock<PathBuf> for MacRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<PathBuf>) -> io::Result<()> {
        for &(off, p) in &self.entries {
            let name = name_at(&self.names, off);
            let bytes = name.to_bytes();
            let meta = match p {
                Pending::Ready(m) => Ok(m),
                Pending::Stat => stat_at(self.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW),
            };
            let child = match &meta {
                Ok(m) if m.kind == Kind::Dir => Some(self.path.join(OsStr::from_bytes(bytes))),
                _ => None,
            };
            out.push(bytes, meta.map_err(|e| ErrKind::from(&e)), child);
        }
        self.entries.clear();
        Ok(())
    }
}

fn stat_at(dir: BorrowedFd<'_>, name: &CStr, flags: AtFlags) -> io::Result<Meta> {
    let st = rustix::fs::statat(dir, name, flags)?;
    Ok(meta_of(&st))
}

/// [`Meta`] as [`Meta::from_metadata`] makes it from the same `stat`.
fn meta_of(st: &rustix::fs::Stat) -> Meta {
    Meta {
        kind: match FileType::from_raw_mode(st.st_mode) {
            FileType::Directory => Kind::Dir,
            FileType::Symlink => Kind::Symlink,
            FileType::RegularFile => Kind::File,
            _ => Kind::Other,
        },
        apparent: st.st_size.cast_unsigned(),
        alloc: st.st_blocks.cast_unsigned() * 512,
        mtime: st.st_mtime.max(0),
        dev: i64::from(st.st_dev).cast_unsigned(),
        ino: st.st_ino,
        nlink: st.st_nlink.into(),
        mount: 0,
    }
}

impl UnixStat for MacBackend {
    /// A symlink as the last component of a directory's path is not followed.
    const OPEN_LAST: OFlags = OFlags::NOFOLLOW;

    fn stat_at(dir: BorrowedFd<'_>, name: &CStr, flags: AtFlags) -> io::Result<Meta> {
        stat_at(dir, name, flags)
    }

    fn stat_fd(fd: BorrowedFd<'_>) -> io::Result<Meta> {
        Ok(meta_of(&rustix::fs::fstat(fd)?))
    }
}

impl MacStream {
    /// One `getattrlistbulk` call, or calls to the end for a drained directory.
    fn fill(&mut self) {
        loop {
            let mut buf = self.common.take();
            match dfdu_sys::read_bulk(self.fd.as_fd(), &mut buf) {
                Ok(0) => {
                    self.common.give(buf);
                    self.done = true;
                    return;
                }
                Ok(n) => {
                    if !self.checked {
                        self.checked = true;
                        // The first record's device is the directory's: only a mount point's
                        // record could differ, and it describes the directory it covers.
                        if let Ok((e, _)) = dfdu_sys::parse_bulk_record(&buf, 0)
                            && let Some(dev) = e.dev
                        {
                            self.drain = self.common.drains(dev, self.fd.as_fd());
                        }
                    }
                    self.filled.push_back(Filled {
                        buf,
                        pos: 0,
                        left: n,
                    });
                    if !self.drain {
                        return;
                    }
                }
                Err(e) => {
                    self.common.give(buf);
                    self.err = Some(e);
                    self.done = true;
                    return;
                }
            }
        }
    }
}

impl DirStream for MacStream {
    type Loc = PathBuf;

    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<PathBuf>, bool)> {
        let mut raw = MacRaw {
            fd: self.fd.clone(),
            path: self.path.clone(),
            names: Vec::new(),
            entries: Vec::new(),
        };
        while raw.entries.len() < max_entries && raw.names.len() < max_name_bytes {
            let Some(f) = self.filled.front_mut() else {
                if self.done {
                    break;
                }
                self.fill();
                continue;
            };
            match dfdu_sys::parse_bulk_record(&f.buf, f.pos) {
                Ok((e, next)) => {
                    raw.add(&e);
                    f.pos = next;
                    f.left -= 1;
                    if f.left == 0
                        && let Some(f) = self.filled.pop_front()
                    {
                        self.common.give(f.buf);
                    }
                }
                Err(e) => {
                    // The records after a malformed one cannot be found: end the listing.
                    for f in self.filled.drain(..) {
                        self.common.give(f.buf);
                    }
                    self.err = Some(e);
                    self.done = true;
                }
            }
        }
        // An error after a partial read is returned on the next call.
        if raw.entries.is_empty()
            && self.filled.is_empty()
            && let Some(e) = self.err.take()
        {
            return Err(e);
        }
        let more = !self.done || !self.filled.is_empty() || self.err.is_some();
        Ok((Box::new(raw), more))
    }
}

impl Backend for MacBackend {
    type Loc = PathBuf;
    type Stream = MacStream;

    /// A root given as a symlink is resolved here, since directories are opened without
    /// following one.
    fn loc_from_path(&self, path: &Path) -> PathBuf {
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            }
            _ => path.to_path_buf(),
        }
    }

    fn open(&self, loc: &PathBuf, _storage: dfdu_sys::Storage) -> io::Result<MacStream> {
        Ok(MacStream {
            fd: Arc::new(super::unix::open_dir(loc, Self::OPEN_LAST)?),
            path: Arc::new(loc.clone()),
            common: self.common.clone(),
            filled: VecDeque::new(),
            checked: false,
            drain: false,
            err: None,
            done: false,
        })
    }

    portable_queries!(loc => loc);
    unix_path_queries!();
}

#[cfg(test)]
mod tests;
