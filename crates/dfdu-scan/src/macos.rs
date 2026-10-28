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

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_sys::{BulkEntry, SF_FIRMLINK, VDIR, VLNK, VREG};
use dfdu_tree::{ErrKind, Kind, Meta, Subvol};
use parking_lot::Mutex;
use rustix::fs::{AtFlags, CWD, FileType, OFlags};

use crate::FsClass;
use crate::backend::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock};
use crate::unix::{name_at, push_name};

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

/// Opens a directory by path without following a symlink in its last component, in pieces
/// when the path is longer than the kernel accepts.
fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    crate::unix::open_dir(path, OFlags::NOFOLLOW)
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
    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        crate::backend::read_prefix_at(dir, name, len)
    }

    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        crate::backend::link_target_missing_at(dir, name)
    }

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            }
            _ => path.to_path_buf(),
        }
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        let name = CString::new(loc.as_os_str().as_bytes())?;
        match stat_at(CWD, &name, AtFlags::empty()) {
            // Too long for one call: stat the directory once it is open.
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NAMETOOLONG.raw_os_error()) => {
                Ok(meta_of(&rustix::fs::fstat(open_dir(loc)?)?))
            }
            r => r,
        }
    }

    fn open(&self, loc: &PathBuf, _storage: dfdu_sys::Storage) -> io::Result<MacStream> {
        Ok(MacStream {
            fd: Arc::new(open_dir(loc)?),
            path: Arc::new(loc.clone()),
            common: self.common.clone(),
            filled: VecDeque::new(),
            checked: false,
            drain: false,
            err: None,
            done: false,
        })
    }

    fn subvolume(&self, loc: &PathBuf) -> io::Result<Option<Subvol>> {
        self.portable.subvolume(loc)
    }

    fn shares_extents(&self, loc: &PathBuf) -> bool {
        self.portable.shares_extents(loc)
    }

    fn data_profile(&self, loc: &PathBuf) -> Option<(&'static str, f64)> {
        self.portable.data_profile(loc)
    }

    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        self.portable.duplicate_mounts(scope)
    }

    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        self.portable.mount_class(mount, dev)
    }

    fn mount_storage(&self, mount: u64, dev: u64) -> dfdu_sys::Storage {
        self.portable.mount_storage(mount, dev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Every entry of a directory as `(name, meta)`, sorted by name, read `per_block` entries
    /// at a time.
    fn list<B: Backend<Loc = PathBuf>>(
        b: &B,
        dir: &Path,
        per_block: usize,
    ) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
        crate::unix::tests::list(b, dir, dfdu_sys::Storage::Unknown, per_block)
    }

    #[test]
    fn entries_match_the_portable_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("sub")).unwrap();
        for i in 0..20 {
            fs::write(root.join(format!("f{i}")), vec![1u8; i * 1000]).unwrap();
        }
        fs::hard_link(root.join("f3"), root.join("sub/link")).unwrap();
        std::os::unix::fs::symlink("sub", root.join("sym")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
        // A sparse file, an empty file, a FIFO and a file with a resource fork (counted in
        // `st_blocks`, not in `st_size`).
        fs::File::create(root.join("sparse"))
            .unwrap()
            .set_len(10 << 20)
            .unwrap();
        fs::write(root.join("empty"), b"").unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(root.join("fifo"))
            .status()
            .unwrap();
        assert!(fifo.success());
        fs::write(root.join("forked"), b"data").unwrap();
        // Where the filesystem has resource forks.
        let _ = fs::write(root.join("forked/..namedfork/rsrc"), vec![2u8; 50_000]);
        let ours = list(&MacBackend::default(), root, 3);
        let theirs = list(&PortableBackend::default(), root, 3);
        assert_eq!(ours.len(), 27);
        assert_eq!(ours, theirs);
        // A directory a buffer cannot hold, in one block and in many.
        let wide = root.join("sub");
        for i in 0..3000 {
            fs::write(wide.join(format!("{i:05}-{}", "w".repeat(100))), b"x").unwrap();
        }
        let theirs = list(&PortableBackend::default(), &wide, 4096);
        assert_eq!(theirs.len(), 3001);
        for per_block in [4096, 1000, 7] {
            assert_eq!(list(&MacBackend::default(), &wide, per_block), theirs);
        }
        let (m, p) = (MacBackend::default(), PortableBackend::default());
        let r = root.to_path_buf();
        assert_eq!(m.stat_root(&r).unwrap(), p.stat_root(&r).unwrap());
        // A symlink given as the root is followed.
        let s = m.loc_from_path(&root.join("sym"));
        assert_eq!(
            m.stat_root(&s).unwrap(),
            p.stat_root(&root.join("sym")).unwrap()
        );
        assert_eq!(list(&m, &s, 100).len(), 3001);
        // Below the root, a symlink is not opened as a directory.
        assert!(
            m.open(&root.join("sym"), dfdu_sys::Storage::Unknown)
                .is_err()
        );
    }

    /// The firmlinks at the top of the system volume (listed in `/usr/share/firmlinks`) are
    /// links, so a scan of `/` does not read the Data volume through them.
    #[test]
    fn firmlinks_are_links() {
        let Ok(text) = fs::read_to_string("/usr/share/firmlinks") else {
            return;
        };
        let top: Vec<&[u8]> = text
            .lines()
            .filter_map(|l| l.split('\t').next()?.strip_prefix('/'))
            .filter(|p| !p.is_empty() && !p.contains('/'))
            .map(str::as_bytes)
            .collect();
        let mut seen = 0;
        for (name, meta) in list(&MacBackend::default(), Path::new("/"), 4096) {
            if top.contains(&&name[..]) {
                let m = meta.unwrap_or_else(|e| panic!("/{}: {e}", name.escape_ascii()));
                assert_eq!(m.kind, Kind::Symlink, "/{}: {m:?}", name.escape_ascii());
                assert_eq!((m.apparent, m.alloc), (0, 0));
                seen += 1;
            }
        }
        // `/Users` and `/Applications`, at least.
        assert!(seen >= 2, "{seen} firmlinks of {top:?}");
    }

    #[test]
    fn unreadable_directory_is_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let e = MacBackend::default()
            .open(&locked, dfdu_sys::Storage::Unknown)
            .err()
            .unwrap();
        assert_eq!(ErrKind::from(&e), ErrKind::PermissionDenied);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
