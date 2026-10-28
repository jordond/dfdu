//! Filesystem access. A backend opens directories and streams their entries in blocks; the
//! portable backend uses `std::fs` and is the reference the fast per-OS backends are tested
//! against.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dfdu_tree::{ErrKind, Kind, Meta, Subvol};

use crate::FsClass;
use crate::proto::NameSpan;

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
/// also be forwarded in the `Forward` impl below.
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
    fn open(&self, loc: &Self::Loc, storage: dfdu_sys::Storage) -> io::Result<Self::Stream>;
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
    fn data_profile(&self, _loc: &Self::Loc) -> Option<(&'static str, f64)> {
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
    fn mount_storage(&self, _mount: u64, _dev: u64) -> dfdu_sys::Storage {
        dfdu_sys::Storage::Unknown
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
        storage: dfdu_sys::Storage,
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
    fn open(&self, loc: &Self::Loc, storage: dfdu_sys::Storage) -> io::Result<W::Stream> {
        Forward::open(self, loc, storage)
    }
    fn subvolume(&self, loc: &Self::Loc) -> io::Result<Option<Subvol>> {
        self.inner().subvolume(loc)
    }
    fn shares_extents(&self, loc: &Self::Loc) -> bool {
        self.inner().shares_extents(loc)
    }
    fn data_profile(&self, loc: &Self::Loc) -> Option<(&'static str, f64)> {
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
    fn mount_storage(&self, mount: u64, dev: u64) -> dfdu_sys::Storage {
        self.inner().mount_storage(mount, dev)
    }
}

/// Mount id of a directory for [`Meta::mount`], 0 when unknown.
fn mount_of(path: &Path) -> u64 {
    dfdu_sys::mount_id(path).ok().flatten().unwrap_or(0)
}

/// `std::fs` backend: one `stat` per entry, no OS-specific fast paths. On Windows, where std
/// has no allocation size, file id or link count, each entry is opened for them instead
/// (`dfdu_sys::stat_path`).
#[derive(Default, Debug)]
pub struct PortableBackend {
    /// Mount classes from the mount table; re-read when an unknown mount appears.
    classes: parking_lot::Mutex<MountClasses>,
}

#[derive(Default, Debug)]
struct MountClasses {
    by_id: std::collections::HashMap<u64, FsClass>,
    by_dev: std::collections::HashMap<u64, FsClass>,
}

impl MountClasses {
    fn get(&self, mount: u64, dev: u64) -> Option<FsClass> {
        if mount != 0 {
            self.by_id.get(&mount).copied()
        } else {
            self.by_dev.get(&dev).copied()
        }
    }
}

pub struct PortableStream {
    iter: fs::ReadDir,
    pending_err: Option<io::Error>,
    done: bool,
}

/// `std::fs` entries; each one stats itself relative to the directory it was read from.
struct PortableRaw(Vec<fs::DirEntry>);

impl RawBlock<PathBuf> for PortableRaw {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn stat(&mut self, out: &mut Block<PathBuf>) -> io::Result<()> {
        #[cfg(windows)]
        let mut volume = None;
        for entry in self.0.drain(..) {
            let name = entry.file_name();
            #[cfg(not(windows))]
            let (mut meta, enter) = (entry.metadata().map(|m| Meta::from_metadata(&m)), true);
            #[cfg(windows)]
            let (mut meta, enter) = match windows_meta(&entry, &mut volume) {
                Ok((m, enter)) => (Ok(m), enter),
                Err(e) => (Err(e), true),
            };
            let child = match &mut meta {
                Ok(m) if m.kind == Kind::Dir && enter => {
                    let path = entry.path();
                    m.mount = mount_of(&path);
                    Some(path)
                }
                _ => None,
            };
            out.push(
                &dfdu_tree::name_bytes(&name),
                meta.map_err(|e| ErrKind::from(&e)),
                child,
            );
        }
        Ok(())
    }
}

/// Metadata of a directory entry on Windows, and whether it may be opened: the entry's kind,
/// write time and attributes from the listing (`FindNextFileW`), the rest from the entry opened
/// with attribute access ([`dfdu_sys::stat_path`]), as std has no allocation size, file id or
/// link count. The listing's size is used for directories and links; files take theirs from
/// the file, as a directory entry's copy can be out of date for a file with several names.
/// Cloud placeholders and offline files are not opened (`WinStat::recalls_on_open`): they get
/// allocation 0 and file id 0, and a placeholder directory is not entered. `volume` caches the
/// directory's volume serial number for them.
#[cfg(windows)]
fn windows_meta(entry: &fs::DirEntry, volume: &mut Option<u32>) -> io::Result<(Meta, bool)> {
    use dfdu_tree::{WinStat, win_attr};
    use std::os::windows::fs::MetadataExt;
    let m = entry.metadata()?;
    let mut w = WinStat {
        attributes: m.file_attributes(),
        // Only the name-surrogate bit decides the kind; the real tag comes with the handle.
        reparse_tag: if m.file_type().is_symlink() {
            win_attr::TAG_NAME_SURROGATE
        } else {
            0
        },
        size: m.len(),
        alloc: 0,
        last_write: m.last_write_time(),
        volume: 0,
        file_id: 0,
        nlink: 1,
    };
    if w.recalls_on_open() {
        w.volume = if let Some(v) = *volume {
            v
        } else {
            let dir = entry.path();
            let v = dfdu_sys::stat_path(dir.parent().unwrap_or(&dir), true)?.volume;
            *volume = Some(v);
            v
        };
        return Ok((Meta::from_windows(&w), false));
    }
    let h = dfdu_sys::stat_path(&entry.path(), false)?;
    w.reparse_tag = h.reparse_tag;
    w.alloc = h.alloc;
    w.volume = h.volume;
    w.file_id = h.file_id;
    if w.kind() == Kind::File {
        w.size = h.size;
        w.nlink = h.nlink;
    }
    Ok((Meta::from_windows(&w), true))
}

impl DirStream for PortableStream {
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
        let mut raw = Vec::new();
        let mut name_bytes = 0;
        while !self.done && raw.len() < max_entries && name_bytes < max_name_bytes {
            match self.iter.next() {
                None => self.done = true,
                Some(Err(e)) => {
                    if raw.is_empty() {
                        self.done = true;
                        return Err(e);
                    }
                    self.pending_err = Some(e);
                    break;
                }
                Some(Ok(entry)) => {
                    name_bytes += entry.file_name().len();
                    raw.push(entry);
                }
            }
        }
        Ok((Box::new(PortableRaw(raw)), !self.done))
    }
}

impl Backend for PortableBackend {
    type Loc = PathBuf;
    type Stream = PortableStream;

    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        read_prefix_at(dir, name, len)
    }

    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        link_target_missing_at(dir, name)
    }

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        #[cfg(not(windows))]
        let mut m = Meta::from_metadata(&fs::metadata(loc)?);
        #[cfg(windows)]
        let mut m = Meta::from_windows(&dfdu_sys::stat_path(loc, true)?);
        m.mount = mount_of(loc);
        Ok(m)
    }

    fn open(&self, loc: &PathBuf, _storage: dfdu_sys::Storage) -> io::Result<PortableStream> {
        Ok(PortableStream {
            iter: fs::read_dir(loc)?,
            pending_err: None,
            done: false,
        })
    }

    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        let table = dfdu_sys::mount_table().ok()?;
        Some(crate::duplicate_mounts(&table, scope))
    }

    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        let mut c = self.classes.lock();
        if c.get(mount, dev).is_none() {
            // A mount made since the table was read: read it again.
            let mut fresh = MountClasses::default();
            for e in dfdu_sys::mount_table().ok()? {
                let class = dfdu_sys::classify_fs(&e.fstype);
                fresh.by_id.insert(e.id, class);
                fresh.by_dev.insert(e.st_dev(), class);
            }
            *c = fresh;
        }
        c.get(mount, dev)
    }

    fn mount_storage(&self, mount: u64, dev: u64) -> dfdu_sys::Storage {
        let Ok(table) = dfdu_sys::mount_table() else {
            return dfdu_sys::Storage::Unknown;
        };
        let entry = table.iter().find(|e| {
            if mount != 0 {
                e.id == mount
            } else {
                e.st_dev() == dev
            }
        });
        entry.map_or(dfdu_sys::Storage::Unknown, dfdu_sys::storage_of_mount)
    }

    fn shares_extents(&self, loc: &PathBuf) -> bool {
        dfdu_sys::shares_extents(loc).unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn data_profile(&self, loc: &PathBuf) -> Option<(&'static str, f64)> {
        let dir = fs::File::open(loc).ok()?;
        let (p, devices) = dfdu_sys::data_profile(&dir).ok()?;
        Some((p.name(), p.raw_ratio(devices)))
    }

    fn subvolume(&self, loc: &PathBuf) -> io::Result<Option<Subvol>> {
        let Some(s) = dfdu_sys::subvol_info(loc)? else {
            return Ok(None);
        };
        // Quota groups need root; without quotas or the privilege there are none.
        #[cfg(target_os = "linux")]
        let qgroup = if dfdu_sys::is_privileged() {
            fs::File::open(loc)
                .ok()
                .and_then(|d| dfdu_sys::qgroup_usage(&d, s.id).ok().flatten())
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let qgroup = None;
        Ok(Some(Subvol {
            id: s.id,
            uuid: s.uuid,
            parent_uuid: s.parent_uuid,
            read_only: s.read_only,
            otime: s.otime,
            qgroup,
        }))
    }
}

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
    use super::*;

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
        fn open(&self, _loc: &PathBuf, _storage: dfdu_sys::Storage) -> io::Result<PortableStream> {
            Err(io::Error::other("inner open"))
        }
        fn subvolume(&self, _loc: &PathBuf) -> io::Result<Option<Subvol>> {
            Err(io::Error::other("subvolume"))
        }
        fn shares_extents(&self, _loc: &PathBuf) -> bool {
            true
        }
        fn data_profile(&self, _loc: &PathBuf) -> Option<(&'static str, f64)> {
            Some(("raid1", 2.0))
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
        fn mount_storage(&self, _mount: u64, _dev: u64) -> dfdu_sys::Storage {
            dfdu_sys::Storage::Rotational
        }
    }

    struct Wrapped(Answers);

    impl Forward for Wrapped {
        type Inner = Answers;
        type Stream = PortableStream;

        fn inner(&self) -> &Answers {
            &self.0
        }
        fn open(&self, _loc: &PathBuf, _storage: dfdu_sys::Storage) -> io::Result<PortableStream> {
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
        let unknown = dfdu_sys::Storage::Unknown;
        assert_eq!(
            b.loc_from_path(Path::new("/d")),
            PathBuf::from("/d/answers")
        );
        assert_eq!(err(b.stat_root(&dir)), "stat_root");
        assert_eq!(err(Backend::open(&b, &dir, unknown)), "wrapper open");
        assert_eq!(err(b.subvolume(&dir)), "subvolume");
        assert!(b.shares_extents(&dir));
        assert_eq!(b.data_profile(&dir), Some(("raid1", 2.0)));
        assert_eq!(
            b.read_prefix(&dir, b"CACHEDIR.TAG", 5),
            Some(b"CACHE".to_vec())
        );
        assert!(b.link_target_missing(&dir, b"link"));
        assert_eq!(b.duplicate_mounts(&dir), Some(HashSet::from([7])));
        assert_eq!(b.mount_class(1, 2), Some(FsClass::Network));
        assert_eq!(b.mount_storage(1, 2), dfdu_sys::Storage::Rotational);
    }
}
