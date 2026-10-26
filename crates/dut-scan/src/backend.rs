//! Filesystem access. A backend opens directories and streams their entries in blocks; the
//! portable backend uses `std::fs` and is the reference the fast per-OS backends are tested
//! against.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dut_tree::{ErrKind, Kind, Meta, Subvol};

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
    pub fn push(&mut self, name: &[u8], meta: Result<Meta, ErrKind>, child: Option<L>) {
        // No filesystem allows a name this long; a truncated name would not convert back to an
        // OsStr, so record the entry under a placeholder instead.
        let (name, meta) = if name.len() > u16::MAX as usize {
            (&b"<name too long>"[..], Err(ErrKind::Other))
        } else {
            (name, meta)
        };
        let len = name.len();
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.entries.push(BlockEntry {
            name: NameSpan {
                off,
                len: len as u16,
            },
            meta,
            child,
        });
    }

    pub fn name(&self, s: NameSpan) -> &[u8] {
        &self.names[s.off as usize..s.off as usize + s.len as usize]
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.entries.clear();
    }
}

pub trait DirStream: Send {
    type Loc;

    /// Appends entries to `out` until `max_entries` entries or `max_name_bytes` of names are in
    /// it, or the directory ends. Returns `Ok(true)` if more entries may follow and `Ok(false)`
    /// at the end. An error after a partial read is returned on the next call, so the entries
    /// already read are kept. `ErrorKind::WouldBlock` means "not ready, call again later"; only
    /// simulated backends return it.
    fn next_block(
        &mut self,
        out: &mut Block<Self::Loc>,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<bool>;
}

pub trait Backend: Send + Sync + 'static {
    type Loc: Send + Clone + std::fmt::Debug + 'static;
    type Stream: DirStream<Loc = Self::Loc>;

    fn loc_from_path(&self, path: &Path) -> Self::Loc;
    /// Stat of a job root, following a symlink given as the root.
    fn stat_root(&self, loc: &Self::Loc) -> io::Result<Meta>;
    fn open(&self, loc: &Self::Loc) -> io::Result<Self::Stream>;
    /// The subvolume record if `loc` is a btrfs subvolume root. Called for job roots and for
    /// directories whose `dev` differs from their parent's.
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
    /// Mount ids of mounts below `scope` that show data reachable inside `scope` through
    /// another mount ([`crate::duplicate_mounts`]). `None` when the system has no mount table;
    /// the scanner then recognises a second mount only by its root's `(dev, ino)`.
    fn duplicate_mounts(&self, _scope: &Path) -> Option<HashSet<u64>> {
        None
    }
}

/// Mount id of a directory for [`Meta::mount`], 0 when unknown.
fn mount_of(path: &Path) -> u64 {
    dut_sys::mount_id(path).ok().flatten().unwrap_or(0)
}

/// `std::fs` backend: one `stat` per entry, no OS-specific fast paths.
#[derive(Default, Clone, Copy, Debug)]
pub struct PortableBackend;

pub struct PortableStream {
    iter: fs::ReadDir,
    pending_err: Option<io::Error>,
    done: bool,
}

impl DirStream for PortableStream {
    type Loc = PathBuf;

    fn next_block(
        &mut self,
        out: &mut Block<PathBuf>,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<bool> {
        if let Some(e) = self.pending_err.take() {
            self.done = true;
            return Err(e);
        }
        if self.done {
            return Ok(false);
        }
        let start = out.entries.len();
        while out.entries.len() - start < max_entries && out.names.len() < max_name_bytes {
            match self.iter.next() {
                None => {
                    self.done = true;
                    return Ok(false);
                }
                Some(Err(e)) => {
                    if out.entries.len() == start {
                        self.done = true;
                        return Err(e);
                    }
                    self.pending_err = Some(e);
                    return Ok(true);
                }
                Some(Ok(entry)) => {
                    let name = entry.file_name();
                    let mut meta = entry.metadata().map(|m| Meta::from_metadata(&m));
                    let child = match &mut meta {
                        Ok(m) if m.kind == Kind::Dir => {
                            let path = entry.path();
                            m.mount = mount_of(&path);
                            Some(path)
                        }
                        _ => None,
                    };
                    out.push(
                        &dut_tree::name_bytes(&name),
                        meta.map_err(|e| ErrKind::from(&e)),
                        child,
                    );
                }
            }
        }
        Ok(true)
    }
}

impl Backend for PortableBackend {
    type Loc = PathBuf;
    type Stream = PortableStream;

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        let mut m = Meta::from_metadata(&fs::metadata(loc)?);
        m.mount = mount_of(loc);
        Ok(m)
    }

    fn open(&self, loc: &PathBuf) -> io::Result<PortableStream> {
        Ok(PortableStream {
            iter: fs::read_dir(loc)?,
            pending_err: None,
            done: false,
        })
    }

    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        let table = dut_sys::mount_table().ok()?;
        Some(crate::duplicate_mounts(&table, scope))
    }

    fn shares_extents(&self, loc: &PathBuf) -> bool {
        dut_sys::shares_extents(loc).unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn data_profile(&self, loc: &PathBuf) -> Option<(&'static str, f64)> {
        let dir = fs::File::open(loc).ok()?;
        let (p, devices) = dut_sys::data_profile(&dir).ok()?;
        Some((p.name(), p.raw_ratio(devices)))
    }

    fn subvolume(&self, loc: &PathBuf) -> io::Result<Option<Subvol>> {
        let Some(s) = dut_sys::subvol_info(loc)? else {
            return Ok(None);
        };
        // Quota groups need root; without quotas or the privilege there are none.
        #[cfg(target_os = "linux")]
        let qgroup = if dut_sys::is_privileged() {
            fs::File::open(loc)
                .ok()
                .and_then(|d| dut_sys::qgroup_usage(&d, s.id).ok().flatten())
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
