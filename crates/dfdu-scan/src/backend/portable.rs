//! The `std::fs` backend, which every platform has and the native backends are tested against.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{ErrKind, Kind, Meta, Subvol};

use super::{Backend, Block, DirStream, Raw, RawBlock, link_target_missing_at, read_prefix_at};

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
    by_id: HashMap<u64, FsClass>,
    by_dev: HashMap<u64, FsClass>,
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

    fn stat_dir(&self, path: &Path) -> io::Result<Meta> {
        #[cfg(not(windows))]
        let m = Meta::from_metadata(&fs::symlink_metadata(path)?);
        #[cfg(windows)]
        let m = Meta::from_windows(&dfdu_sys::stat_path(path, false)?);
        Ok(m)
    }

    fn open(&self, loc: &PathBuf, _storage: Storage) -> io::Result<PortableStream> {
        Ok(PortableStream {
            iter: fs::read_dir(loc)?,
            pending_err: None,
            done: false,
        })
    }

    fn duplicate_mounts(&self, scope: &Path) -> Option<HashSet<u64>> {
        let table = dfdu_sys::mount_table().ok()?;
        Some(crate::overlap::duplicate_mounts(&table, scope))
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

    fn mount_storage(&self, mount: u64, dev: u64) -> Storage {
        let Ok(table) = dfdu_sys::mount_table() else {
            return Storage::Unknown;
        };
        let entry = table.iter().find(|e| {
            if mount != 0 {
                e.id == mount
            } else {
                e.st_dev() == dev
            }
        });
        entry.map_or(Storage::Unknown, dfdu_sys::storage_of_mount)
    }

    fn shares_extents(&self, loc: &PathBuf) -> bool {
        dfdu_sys::shares_extents(loc).unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn data_profile(&self, loc: &PathBuf) -> Option<(dfdu_sys::Profile, f64)> {
        let dir = fs::File::open(loc).ok()?;
        let (p, devices) = dfdu_sys::data_profile(&dir).ok()?;
        Some((p, p.raw_ratio(devices)))
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
