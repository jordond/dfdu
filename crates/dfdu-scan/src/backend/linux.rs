//! Linux backend: `getdents64` into a buffer per directory, and one `statx` per entry relative
//! to the directory's descriptor, which raw blocks share so other workers can stat parts of a
//! wide directory. The mount id comes in the same `statx` call, where
//! the portable backend needs a second, full-path call for every directory. Directories are
//! opened by full path, and paths longer than `PATH_MAX` are opened a piece at a time with
//! `openat`, so trees of any depth are read.
//!
//! The calls that happen once per directory at most (mount table, subvolume and extent probes)
//! are the portable backend's.

use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dfdu_tree::{ErrKind, Kind, Meta};
use rustix::fs::{AtFlags, CWD, FileType, OFlags, RawDir, StatxFlags};

use super::unix::{name_at, push_name};
use super::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock, portable_queries};

const MASK: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::MODE)
    .union(StatxFlags::NLINK)
    .union(StatxFlags::INO)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::BLOCKS)
    .union(StatxFlags::MTIME)
    .union(StatxFlags::MNT_ID);

#[derive(Default, Debug)]
pub struct LinuxBackend {
    portable: PortableBackend,
    /// Whether directories on a mount whose storage is not known are read in inode order: the
    /// scan root's storage.
    inode_order: bool,
}

impl LinuxBackend {
    /// The backend for a scan on `storage`. A directory's entries are `statx`ed in inode
    /// order on a spinning disk, which follows the inode tables' order on the disk instead of
    /// seeking back and forth in name-hash order, and in directory order otherwise. The order
    /// follows the storage of the directory's own mount, which the scanner gives to
    /// [`Backend::open`]; `storage` decides only for a mount whose storage is not known.
    #[must_use]
    pub fn for_storage(storage: dfdu_sys::Storage) -> Self {
        LinuxBackend {
            portable: PortableBackend::default(),
            inode_order: storage == dfdu_sys::Storage::Rotational,
        }
    }
}

pub struct LinuxStream {
    /// Shared with the raw blocks, which stat relative to it, possibly on other workers.
    fd: Arc<OwnedFd>,
    path: Arc<PathBuf>,
    buf: Vec<MaybeUninit<u8>>,
    pending_err: Option<io::Error>,
    done: bool,
    inode_order: bool,
}

/// Names read by `getdents64`, NUL-terminated in one buffer, with their inode numbers.
struct LinuxRaw {
    fd: Arc<OwnedFd>,
    path: Arc<PathBuf>,
    names: Vec<u8>,
    /// Start of each name in `names` (it ends at its NUL), and its inode number.
    entries: Vec<(u32, u64)>,
    inode_order: bool,
}

impl RawBlock<PathBuf> for LinuxRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<PathBuf>) -> io::Result<()> {
        if self.inode_order {
            self.entries.sort_unstable_by_key(|e| e.1);
        }
        for &(off, _) in &self.entries {
            let name = name_at(&self.names, off);
            let bytes = name.to_bytes();
            let meta = stat_at(self.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW);
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

/// `statx` (with `flags`, plus no automount), falling back to `fstatat` on kernels before 4.11
/// (no mount id then).
fn stat_at(dir: BorrowedFd<'_>, name: &CStr, flags: AtFlags) -> io::Result<Meta> {
    match rustix::fs::statx(dir, name, flags | AtFlags::NO_AUTOMOUNT, MASK) {
        Ok(st) => {
            let kind = kind(FileType::from_raw_mode(st.stx_mode.into()));
            let has_mount = st.stx_mask & StatxFlags::MNT_ID.bits() != 0;
            Ok(Meta {
                kind,
                apparent: st.stx_size,
                alloc: st.stx_blocks * 512,
                mtime: st.stx_mtime.tv_sec.max(0),
                dev: rustix::fs::makedev(st.stx_dev_major, st.stx_dev_minor),
                ino: st.stx_ino,
                nlink: st.stx_nlink,
                // Only directories carry their mount id, as in the portable backend.
                mount: if kind == Kind::Dir && has_mount {
                    st.stx_mnt_id
                } else {
                    0
                },
            })
        }
        Err(rustix::io::Errno::NOSYS) => {
            let st = rustix::fs::statat(dir, name, flags)?;
            Ok(Meta {
                kind: kind(FileType::from_raw_mode(st.st_mode)),
                apparent: st.st_size.cast_unsigned(),
                alloc: st.st_blocks.cast_unsigned() * 512,
                mtime: (st.st_mtime as i64).max(0),
                dev: st.st_dev,
                ino: st.st_ino,
                nlink: u32::try_from(st.st_nlink).unwrap_or(u32::MAX),
                mount: 0,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn kind(t: FileType) -> Kind {
    match t {
        FileType::Directory => Kind::Dir,
        FileType::Symlink => Kind::Symlink,
        FileType::RegularFile => Kind::File,
        _ => Kind::Other,
    }
}

/// Opens a directory by path, in pieces when the path is longer than the kernel accepts.
fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    super::unix::open_dir(path, OFlags::empty())
}

/// `path`'s metadata, by path, or once it is open when the path is too long for one call.
fn stat_path(path: &Path, flags: AtFlags) -> io::Result<Meta> {
    super::unix::stat_path(
        path,
        |name| stat_at(CWD, name, flags),
        |path| stat_at(open_dir(path)?.as_fd(), c"", AtFlags::EMPTY_PATH),
    )
}

/// `getdents64` buffer per open directory.
const GETDENTS_BUF: usize = 32 * 1024;

impl DirStream for LinuxStream {
    type Loc = PathBuf;

    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<PathBuf>, bool)> {
        if let Some(e) = self.pending_err.take() {
            self.done = true;
            return Err(e);
        }
        let mut raw = LinuxRaw {
            fd: self.fd.clone(),
            path: self.path.clone(),
            names: Vec::new(),
            entries: Vec::new(),
            inode_order: self.inode_order,
        };
        if self.done {
            return Ok((Box::new(raw), false));
        }
        if self.buf.is_empty() {
            self.buf.resize(GETDENTS_BUF, MaybeUninit::uninit());
        }
        let mut dir = RawDir::new(&*self.fd, &mut self.buf);
        // Where the next call must continue when this one stops inside the kernel's buffer.
        let mut resume = None;
        while raw.entries.len() < max_entries && raw.names.len() < max_name_bytes {
            match dir.next() {
                None => {
                    self.done = true;
                    resume = None;
                    break;
                }
                Some(Err(e)) => {
                    if raw.entries.is_empty() {
                        self.done = true;
                        return Err(e.into());
                    }
                    self.pending_err = Some(e.into());
                    resume = None;
                    break;
                }
                Some(Ok(entry)) => {
                    let name = entry.file_name();
                    resume = Some(entry.next_entry_cookie());
                    if name.to_bytes() == b"." || name.to_bytes() == b".." {
                        continue;
                    }
                    let off = push_name(&mut raw.names, name.to_bytes());
                    raw.entries.push((off, entry.ino()));
                }
            }
        }
        // Entries left in the buffer would be lost with it: go back to the first of them.
        if let Some(cookie) = resume
            && !dir.is_buffer_empty()
        {
            rustix::fs::seek(&*self.fd, rustix::fs::SeekFrom::Start(cookie))?;
        }
        Ok((Box::new(raw), !self.done))
    }
}

impl Backend for LinuxBackend {
    type Loc = PathBuf;
    type Stream = LinuxStream;

    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        super::read_prefix_at(dir, name, len)
    }

    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        super::link_target_missing_at(dir, name)
    }

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        stat_path(loc, AtFlags::empty())
    }

    fn stat_dir(&self, path: &Path) -> io::Result<Meta> {
        stat_path(path, AtFlags::SYMLINK_NOFOLLOW)
    }

    fn open(&self, loc: &PathBuf, storage: dfdu_sys::Storage) -> io::Result<LinuxStream> {
        let inode_order = match storage {
            dfdu_sys::Storage::Rotational => true,
            dfdu_sys::Storage::Solid | dfdu_sys::Storage::Network => false,
            dfdu_sys::Storage::Unknown => self.inode_order,
        };
        Ok(LinuxStream {
            fd: Arc::new(open_dir(loc)?),
            path: Arc::new(loc.clone()),
            buf: Vec::new(),
            pending_err: None,
            done: false,
            inode_order,
        })
    }

    portable_queries!(loc => loc);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Every entry of a directory as `(name, meta)`, sorted by name, read three at a time.
    fn list<B: Backend<Loc = PathBuf>>(
        b: &B,
        dir: &Path,
        storage: dfdu_sys::Storage,
    ) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
        crate::backend::tests::list(b, dir, storage, 3)
    }

    #[test]
    fn entries_match_the_portable_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        crate::backend::tests::populate(root);
        std::os::unix::fs::symlink("sub", root.join("sym")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
        fs::write(root.join(OsStr::from_bytes(b"bad-\xff")), b"x").unwrap();
        let solid = dfdu_sys::Storage::Solid;
        let ours = list(&LinuxBackend::default(), root, solid);
        let theirs = list(&PortableBackend::default(), root, solid);
        assert_eq!(ours.len(), 24);
        assert_eq!(ours, theirs);
        let hdd = list(
            &LinuxBackend::for_storage(dfdu_sys::Storage::Rotational),
            root,
            dfdu_sys::Storage::Rotational,
        );
        assert_eq!(hdd, theirs);
        let (l, p) = (LinuxBackend::default(), PortableBackend::default());
        let r = root.to_path_buf();
        assert_eq!(l.stat_root(&r).unwrap(), p.stat_root(&r).unwrap());
        // A symlink given as the root is followed.
        let s = root.join("sym");
        assert_eq!(l.stat_root(&s).unwrap(), p.stat_root(&s).unwrap());
    }

    /// Inode numbers of a directory's entries in the order one block `stat`s them.
    fn stat_order(b: &LinuxBackend, dir: &Path, storage: dfdu_sys::Storage) -> Vec<u64> {
        let mut s = b.open(&dir.to_path_buf(), storage).unwrap();
        let mut block = Block::default();
        let (mut raw, more) = s.next_raw(1 << 20, 1 << 20).unwrap();
        assert!(!more);
        raw.stat(&mut block).unwrap();
        block.entries.iter().map(|e| e.meta.unwrap().ino).collect()
    }

    /// The stat order follows the storage of the directory's mount, which the scanner gives
    /// to `open`, whatever the storage the backend was set up for (the scan root's).
    #[test]
    fn stat_order_follows_the_storage_of_the_mount() {
        use dfdu_sys::Storage::{Network, Rotational, Solid, Unknown};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..200 {
            fs::write(root.join(format!("f{}", (i * 37) % 200)), b"x").unwrap();
        }
        let dir_order: Vec<u64> = {
            use std::os::unix::fs::DirEntryExt;
            fs::read_dir(root)
                .unwrap()
                .map(|e| e.unwrap().ino())
                .collect()
        };
        let ascending = |v: &[u64]| v.windows(2).all(|w| w[0] < w[1]);
        if ascending(&dir_order) {
            // A filesystem that lists in inode order (btrfs, say): the orders are the same.
            eprintln!("skipped: the directory lists in inode order here");
            return;
        }
        for root_storage in [Solid, Rotational, Unknown] {
            let b = LinuxBackend::for_storage(root_storage);
            // An HDD mount: inode order. An SSD (or network) mount: directory order.
            assert!(
                ascending(&stat_order(&b, root, Rotational)),
                "{root_storage:?}"
            );
            for other in [Solid, Network] {
                assert_eq!(stat_order(&b, root, other), dir_order, "{root_storage:?}");
            }
        }
        // A mount of unknown storage is read as the scan root's is.
        let hdd_root = LinuxBackend::for_storage(Rotational);
        assert!(ascending(&stat_order(&hdd_root, root, Unknown)));
        let ssd_root = LinuxBackend::for_storage(Solid);
        assert_eq!(stat_order(&ssd_root, root, Unknown), dir_order);
    }
}
