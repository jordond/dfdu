//! Filesystem access. A backend opens directories and streams their entries in blocks; the
//! portable backend uses `std::fs` and is the reference the fast per-OS backends are tested
//! against.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use dfdu_sys::{FsClass, Profile, Storage};
use dfdu_tree::{ErrKind, Meta, Subvol};

use crate::proto::NameSpan;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub mod ntfs;
mod portable;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{LinuxBackend, LinuxStream};
#[cfg(target_os = "macos")]
pub use macos::{MacBackend, MacStream};
pub use portable::PortableBackend;
#[cfg(windows)]
pub use windows::{WinLoc, WindowsBackend, WindowsStream, check_mft};

/// One entry read from a directory.
#[derive(Debug)]
pub struct BlockEntry<L> {
    pub name: NameSpan,
    pub meta: Result<Meta, ErrKind>,
    /// Location of the entry, for directories, so a task can be created for it.
    pub child: Option<L>,
}

/// A block of entries with their names in one buffer.
#[derive(Debug)]
pub struct Block<L> {
    pub names: Vec<u8>,
    pub entries: Vec<BlockEntry<L>>,
}

impl<L> Default for Block<L> {
    fn default() -> Self {
        Block {
            names: Vec::new(),
            entries: Vec::new(),
        }
    }
}

impl<L> Block<L> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a block holds at most `Limits::block_name_bytes` of names, far below `u32::MAX`"
    )]
    pub fn push(&mut self, name: &[u8], meta: Result<Meta, ErrKind>, child: Option<L>) {
        crate::late::yield_if_applier_late(self.entries.len());
        // No filesystem allows a name this long; a truncated name would not convert back to an
        // OsStr, so record the entry under a placeholder instead.
        let (name, meta) = if name.len() > u16::MAX as usize {
            (&b"<name too long>"[..], Err(ErrKind::Other))
        } else {
            (name, meta)
        };
        let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.entries.push(BlockEntry {
            name: NameSpan { off, len },
            meta,
            child,
        });
    }

    #[must_use]
    pub fn name(&self, s: NameSpan) -> &[u8] {
        &self.names[s.off as usize..s.off as usize + s.len as usize]
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.entries.clear();
    }
}

/// Entries read from a directory and not yet stat'ed. The worker reading the directory stats
/// them itself, or hands them to a chunk task another worker runs, so a wide directory is
/// stat'ed by several workers at once.
pub trait RawBlock<L>: Send {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stats every entry into `out`, in any order. Entries that cannot be stat'ed are recorded
    /// with their error. `ErrorKind::WouldBlock` means "not ready, call again later" (simulated
    /// backends only); the next call continues where this one stopped.
    ///
    /// # Errors
    ///
    /// `WouldBlock` when not ready; an error that ends the block early otherwise.
    fn stat(&mut self, out: &mut Block<L>) -> io::Result<()>;
}

pub type Raw<L> = Box<dyn RawBlock<L>>;

pub trait DirStream: Send {
    type Loc;

    /// Reads entries, without stat'ing them, until `max_entries` entries or `max_name_bytes`
    /// of names, or the directory ends. The flag is `true` if more entries may follow and
    /// `false` at the end. An error after a partial read is returned on the next call, so the
    /// entries already read are kept. `ErrorKind::WouldBlock` means "not ready, call again
    /// later"; only simulated backends return it.
    ///
    /// # Errors
    ///
    /// The directory could not be read further, or `WouldBlock` (simulated backends).
    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<Self::Loc>, bool)>;
}

/// Filesystem access for the scanner. A wrapper that changes one behaviour of another backend
/// implements [`Forward`] instead, which forwards every other method. A method added here must
/// also be forwarded in the `Forward` impl below, and, if the native backends answer it like
/// the portable one, in `portable_queries!`.
pub trait Backend: Send + Sync + 'static {
    type Loc: Send + Clone + std::fmt::Debug + 'static;
    type Stream: DirStream<Loc = Self::Loc>;

    fn loc_from_path(&self, path: &Path) -> Self::Loc;
    /// Stat of a job root, following a symlink given as the root.
    ///
    /// # Errors
    ///
    /// The root could not be stat'ed.
    fn stat_root(&self, loc: &Self::Loc) -> io::Result<Meta>;
    /// Opens a directory for listing. `storage` is what the directory's mount is stored on
    /// ([`Backend::mount_storage`], read once when the scan enters the mount, `Unknown` when
    /// not known), so a backend can read the directory in the order that suits it (inode order
    /// on a spinning disk). A backend that has no use for it ignores it.
    ///
    /// # Errors
    ///
    /// The directory could not be opened.
    fn open(&self, loc: &Self::Loc, storage: Storage) -> io::Result<Self::Stream>;
    /// The subvolume record if `loc` is a btrfs subvolume root. Called for job roots and for
    /// directories whose `dev` differs from their parent's.
    ///
    /// # Errors
    ///
    /// The filesystem could not be asked.
    fn subvolume(&self, _loc: &Self::Loc) -> io::Result<Option<Subvol>> {
        Ok(None)
    }
    /// Whether files on `loc`'s filesystem can share extents. Called with
    /// [`Backend::subvolume`].
    fn shares_extents(&self, _loc: &Self::Loc) -> bool {
        false
    }
    /// btrfs data profile of `loc`'s filesystem. Called with [`Backend::subvolume`].
    fn data_profile(&self, _loc: &Self::Loc) -> Option<(Profile, f64)> {
        None
    }
    /// The first `len` bytes (fewer if the file is shorter) of the regular file `name` in the
    /// directory at `dir`, for rule markers such as CACHEDIR.TAG; `None` when it cannot be read.
    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        let _ = (dir, name, len);
        None
    }
    /// Whether the symbolic link `name` in the directory at `dir` points at nothing, for rules
    /// on broken links; `false` when that cannot be told.
    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        let _ = (dir, name);
        false
    }
    /// Mount ids of mounts below `scope` that show data reachable inside `scope` through
    /// another mount ([`crate::duplicate_mounts`]). `None` when the system has no mount table;
    /// the scanner then recognises a second mount only by its root's `(dev, ino)`.
    fn duplicate_mounts(&self, _scope: &Path) -> Option<HashSet<u64>> {
        None
    }
    /// What kind of filesystem a mount root is, from the mount table, without touching the
    /// mount itself (a network mount may not answer): by `mount` id (a [`Meta::mount`]), or by
    /// device `dev` when there is no id. `None` when not known.
    fn mount_class(&self, _mount: u64, _dev: u64) -> Option<FsClass> {
        None
    }
    /// What a mount's data is stored on ([`dfdu_sys::storage`]), for its concurrency limit: by
    /// `mount` id, or by device `dev` when there is no id. Answered from the mount table and
    /// sysfs, without touching the mount. Called once per mount.
    fn mount_storage(&self, _mount: u64, _dev: u64) -> Storage {
        Storage::Unknown
    }
}

/// A backend that wraps another and changes how directories are opened (to delay, gate or
/// panic in tests and test hooks). Every other [`Backend`] method goes to [`Forward::inner`],
/// so a wrapper cannot leave one at its default by leaving it out.
pub trait Forward: Send + Sync + 'static {
    type Inner: Backend;
    type Stream: DirStream<Loc = <Self::Inner as Backend>::Loc>;

    fn inner(&self) -> &Self::Inner;
    /// [`Backend::open`] of the wrapper.
    ///
    /// # Errors
    ///
    /// The directory could not be opened.
    fn open(
        &self,
        loc: &<Self::Inner as Backend>::Loc,
        storage: Storage,
    ) -> io::Result<Self::Stream>;
}

// Every method is written out, defaults included: one left out would give the trait default
// instead of the wrapped backend's answer.
impl<W: Forward> Backend for W {
    type Loc = <W::Inner as Backend>::Loc;
    type Stream = W::Stream;

    fn loc_from_path(&self, path: &Path) -> Self::Loc {
        self.inner().loc_from_path(path)
    }
    fn stat_root(&self, loc: &Self::Loc) -> io::Result<Meta> {
        self.inner().stat_root(loc)
    }
    fn open(&self, loc: &Self::Loc, storage: Storage) -> io::Result<W::Stream> {
        Forward::open(self, loc, storage)
    }
    fn subvolume(&self, loc: &Self::Loc) -> io::Result<Option<Subvol>> {
        self.inner().subvolume(loc)
    }
    fn shares_extents(&self, loc: &Self::Loc) -> bool {
        self.inner().shares_extents(loc)
    }
    fn data_profile(&self, loc: &Self::Loc) -> Option<(Profile, f64)> {
        self.inner().data_profile(loc)
    }
    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        self.inner().read_prefix(dir, name, len)
    }
    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        self.inner().link_target_missing(dir, name)
    }
    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        self.inner().duplicate_mounts(scope)
    }
    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        self.inner().mount_class(mount, dev)
    }
    fn mount_storage(&self, mount: u64, dev: u64) -> Storage {
        self.inner().mount_storage(mount, dev)
    }
}

/// The [`Backend`] methods the native backends answer like [`PortableBackend`]: the mount
/// table, subvolume and extent queries, asked at most once per directory. Expands inside a
/// native backend's `impl Backend`, forwarding to its `portable` field; `$loc => $path` turns
/// the backend's location into the portable one's. Every native backend uses this one list, so
/// none can leave one of these methods at the trait default by leaving it out.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
macro_rules! portable_queries {
    ($loc:ident => $path:expr) => {
        fn subvolume(&self, $loc: &Self::Loc) -> std::io::Result<Option<dfdu_tree::Subvol>> {
            self.portable.subvolume($path)
        }
        fn shares_extents(&self, $loc: &Self::Loc) -> bool {
            self.portable.shares_extents($path)
        }
        fn data_profile(&self, $loc: &Self::Loc) -> Option<(dfdu_sys::Profile, f64)> {
            self.portable.data_profile($path)
        }
        fn duplicate_mounts(
            &self,
            scope: &std::path::Path,
        ) -> Option<std::collections::HashSet<u64>> {
            self.portable.duplicate_mounts(scope)
        }
        fn mount_class(&self, mount: u64, dev: u64) -> Option<dfdu_sys::FsClass> {
            self.portable.mount_class(mount, dev)
        }
        fn mount_storage(&self, mount: u64, dev: u64) -> dfdu_sys::Storage {
            self.portable.mount_storage(mount, dev)
        }
    };
}
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use portable_queries;

/// [`Backend::link_target_missing`] for backends whose location is a path.
pub(crate) fn link_target_missing_at(dir: &Path, name: &[u8]) -> bool {
    fs::metadata(dir.join(dfdu_tree::name_os(name)))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// [`Backend::read_prefix`] for backends whose location is a path.
pub(crate) fn read_prefix_at(dir: &Path, name: &[u8], len: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = fs::File::open(dir.join(dfdu_tree::name_os(name)))
        .ok()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))?;
    let mut buf = Vec::with_capacity(len);
    f.take(len as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::portable::PortableStream;
    use super::*;
    use std::path::PathBuf;

    /// Answers every defaulted method with something other than its default.
    struct Answers;

    impl Backend for Answers {
        type Loc = PathBuf;
        type Stream = PortableStream;

        fn loc_from_path(&self, path: &Path) -> PathBuf {
            path.join("answers")
        }
        fn stat_root(&self, _loc: &PathBuf) -> io::Result<Meta> {
            Err(io::Error::other("stat_root"))
        }
        fn open(&self, _loc: &PathBuf, _storage: Storage) -> io::Result<PortableStream> {
            Err(io::Error::other("inner open"))
        }
        fn subvolume(&self, _loc: &PathBuf) -> io::Result<Option<Subvol>> {
            Err(io::Error::other("subvolume"))
        }
        fn shares_extents(&self, _loc: &PathBuf) -> bool {
            true
        }
        fn data_profile(&self, _loc: &PathBuf) -> Option<(Profile, f64)> {
            Some((Profile::Raid1, 2.0))
        }
        fn read_prefix(&self, _dir: &PathBuf, name: &[u8], len: usize) -> Option<Vec<u8>> {
            Some(name[..len].to_vec())
        }
        fn link_target_missing(&self, _dir: &PathBuf, _name: &[u8]) -> bool {
            true
        }
        fn duplicate_mounts(&self, _scope: &Path) -> Option<HashSet<u64>> {
            Some(HashSet::from([7]))
        }
        fn mount_class(&self, _mount: u64, _dev: u64) -> Option<FsClass> {
            Some(FsClass::Network)
        }
        fn mount_storage(&self, _mount: u64, _dev: u64) -> Storage {
            Storage::Rotational
        }
    }

    struct Wrapped(Answers);

    impl Forward for Wrapped {
        type Inner = Answers;
        type Stream = PortableStream;

        fn inner(&self) -> &Answers {
            &self.0
        }
        fn open(&self, _loc: &PathBuf, _storage: Storage) -> io::Result<PortableStream> {
            Err(io::Error::other("wrapper open"))
        }
    }

    fn err<T>(r: io::Result<T>) -> String {
        r.err().expect("an error").to_string()
    }

    #[test]
    fn forward_answers_like_the_inner_backend_except_open() {
        let b = Wrapped(Answers);
        let dir = PathBuf::from("/d");
        let unknown = Storage::Unknown;
        assert_eq!(
            b.loc_from_path(Path::new("/d")),
            PathBuf::from("/d/answers")
        );
        assert_eq!(err(b.stat_root(&dir)), "stat_root");
        assert_eq!(err(Backend::open(&b, &dir, unknown)), "wrapper open");
        assert_eq!(err(b.subvolume(&dir)), "subvolume");
        assert!(b.shares_extents(&dir));
        assert_eq!(b.data_profile(&dir), Some((Profile::Raid1, 2.0)));
        assert_eq!(
            b.read_prefix(&dir, b"CACHEDIR.TAG", 5),
            Some(b"CACHE".to_vec())
        );
        assert!(b.link_target_missing(&dir, b"link"));
        assert_eq!(b.duplicate_mounts(&dir), Some(HashSet::from([7])));
        assert_eq!(b.mount_class(1, 2), Some(FsClass::Network));
        assert_eq!(b.mount_storage(1, 2), Storage::Rotational);
    }
}
