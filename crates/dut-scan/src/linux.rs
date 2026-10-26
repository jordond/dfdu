//! Linux backend (ROADMAP M2): `getdents64` into a buffer per directory, and one `statx` per
//! entry relative to the directory's descriptor, which raw blocks share so other workers can
//! stat parts of a wide directory. The mount id comes in the same `statx` call, where
//! the portable backend needs a second, full-path call for every directory. Directories are
//! opened by full path, and paths longer than `PATH_MAX` are opened a piece at a time with
//! `openat`, so trees of any depth are read.
//!
//! The calls that happen once per directory at most (mount table, subvolume and extent probes)
//! are the portable backend's.

use std::collections::HashSet;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use dut_tree::{ErrKind, Kind, Meta, Subvol};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, RawDir, StatxFlags};

use crate::FsClass;
use crate::backend::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock};

const MASK: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::MODE)
    .union(StatxFlags::NLINK)
    .union(StatxFlags::INO)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::BLOCKS)
    .union(StatxFlags::MTIME)
    .union(StatxFlags::MNT_ID);

/// Paths are opened in pieces of at most this many bytes when the whole is too long.
const PIECE: usize = 2048;

#[derive(Default, Debug)]
pub struct LinuxBackend {
    portable: PortableBackend,
    inode_order: bool,
}

impl LinuxBackend {
    /// The backend for data on `storage`: on a spinning disk each block of entries is
    /// `statx`ed in inode order, which follows the inode tables' order on the disk instead of
    /// seeking back and forth in name-hash order.
    pub fn for_storage(storage: dut_sys::Storage) -> Self {
        LinuxBackend {
            portable: PortableBackend::default(),
            inode_order: storage == dut_sys::Storage::Rotational,
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
            let rest = &self.names[off as usize..];
            let end = rest.iter().position(|&b| b == 0).expect("NUL-terminated");
            let name = CStr::from_bytes_with_nul(&rest[..=end]).expect("one NUL");
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
                apparent: st.st_size as u64,
                alloc: st.st_blocks as u64 * 512,
                mtime: (st.st_mtime as i64).max(0),
                dev: st.st_dev,
                ino: st.st_ino,
                nlink: st.st_nlink as u32,
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
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match rustix::fs::open(path, flags, Mode::empty()) {
        Err(rustix::io::Errno::NAMETOOLONG) => {}
        r => return r.map_err(Into::into),
    }
    let mut fd: Option<OwnedFd> = None;
    let mut piece = PathBuf::new();
    for c in path.components() {
        let part: &OsStr = match c {
            Component::RootDir => OsStr::new("/"),
            Component::CurDir => continue,
            Component::ParentDir => OsStr::new(".."),
            Component::Normal(n) => n,
            Component::Prefix(_) => unreachable!("no path prefixes on Linux"),
        };
        if !piece.as_os_str().is_empty() && piece.as_os_str().len() + part.len() + 1 > PIECE {
            let base = fd.as_ref().map_or(CWD, |f| f.as_fd());
            fd = Some(rustix::fs::openat(base, &piece, flags, Mode::empty())?);
            piece.clear();
        }
        piece.push(part);
    }
    let base = fd.as_ref().map_or(CWD, |f| f.as_fd());
    Ok(rustix::fs::openat(base, &piece, flags, Mode::empty())?)
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
                    raw.entries.push((raw.names.len() as u32, entry.ino()));
                    raw.names.extend_from_slice(name.to_bytes_with_nul());
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

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        let name = CString::new(loc.as_os_str().as_bytes())?;
        match stat_at(CWD, &name, AtFlags::empty()) {
            // Too long for one call: stat the directory once it is open.
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NAMETOOLONG.raw_os_error()) => {
                stat_at(open_dir(loc)?.as_fd(), c"", AtFlags::EMPTY_PATH)
            }
            r => r,
        }
    }

    fn open(&self, loc: &PathBuf) -> io::Result<LinuxStream> {
        Ok(LinuxStream {
            fd: Arc::new(open_dir(loc)?),
            path: Arc::new(loc.clone()),
            buf: Vec::new(),
            pending_err: None,
            done: false,
            inode_order: self.inode_order,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Every entry of a directory as `(name, meta)`, sorted by name.
    fn list<B: Backend<Loc = PathBuf>>(b: &B, dir: &Path) -> Vec<(Vec<u8>, Result<Meta, ErrKind>)> {
        let mut s = b.open(&dir.to_path_buf()).unwrap();
        let mut block = Block::default();
        // A small block size, so entries are read across several calls (and seeks).
        loop {
            let (mut raw, more) = s.next_raw(3, 1 << 20).unwrap();
            raw.stat(&mut block).unwrap();
            if !more {
                break;
            }
        }
        let mut out: Vec<_> = block
            .entries
            .iter()
            .map(|e| (block.name(e.name).to_vec(), e.meta))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
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
        fs::write(root.join(OsStr::from_bytes(b"bad-\xff")), b"x").unwrap();
        let ours = list(&LinuxBackend::default(), root);
        let theirs = list(&PortableBackend::default(), root);
        assert_eq!(ours.len(), 24);
        assert_eq!(ours, theirs);
        let hdd = list(
            &LinuxBackend::for_storage(dut_sys::Storage::Rotational),
            root,
        );
        assert_eq!(hdd, theirs);
        let (l, p) = (LinuxBackend::default(), PortableBackend::default());
        let r = root.to_path_buf();
        assert_eq!(l.stat_root(&r).unwrap(), p.stat_root(&r).unwrap());
        // A symlink given as the root is followed.
        let s = root.join("sym");
        assert_eq!(l.stat_root(&s).unwrap(), p.stat_root(&s).unwrap());
    }

    #[test]
    fn opens_paths_longer_than_path_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().to_path_buf();
        // Created one level at a time through descriptors, as no single path call reaches it.
        let mut fd = open_dir(&deep).unwrap();
        for i in 0..30 {
            let name = format!("{i:03}-{}", "y".repeat(200));
            rustix::fs::mkdirat(&fd, name.as_str(), Mode::from_raw_mode(0o755)).unwrap();
            fd = rustix::fs::openat(&fd, name.as_str(), OFlags::DIRECTORY, Mode::empty()).unwrap();
            deep.push(name);
        }
        let f = rustix::fs::openat(
            &fd,
            "leaf",
            OFlags::CREATE | OFlags::WRONLY,
            Mode::from_raw_mode(0o644),
        )
        .unwrap();
        rustix::io::write(&f, &[7u8; 99]).unwrap();
        assert!(deep.as_os_str().len() > 4096);
        let b = LinuxBackend::default();
        let entries = list(&b, &deep);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.unwrap().apparent, 99);
        let root = b.stat_root(&deep).unwrap();
        assert_eq!(root.kind, Kind::Dir);
        // Removed level by level from the top would also fail on the long paths.
        drop(fd);
        let _ = std::process::Command::new("rm")
            .arg("-rf")
            .arg(tmp.path().join(format!("000-{}", "y".repeat(200))))
            .status();
    }
}
