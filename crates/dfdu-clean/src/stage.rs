//! Staging, and putting items back. A staged item is renamed into this session's staging
//! directory on its own filesystem: `stage/<session>/` beside the
//! journals when that is on the item's filesystem, else `.dfdu-stage/<session>/` in the scan
//! root or, failing that, next to the item. Every rename that puts something back refuses to
//! replace what is at the target.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dfdu_tree::Meta;

use crate::{Identity, PlanItem};

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` exists, atomically where the
/// system can (`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS,
/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows).
///
/// # Errors
///
/// `AlreadyExists` when `to` exists; any other error of the rename.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }
    #[cfg(windows)]
    {
        dfdu_sys::rename_noreplace(from, to)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        // No atomic form here: a hard link fails if the target exists, but directories cannot
        // be linked, so those are checked first (a narrow race).
        if fs::symlink_metadata(to).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        fs::rename(from, to)
    }
}

fn meta(path: &Path) -> io::Result<Meta> {
    #[cfg(not(windows))]
    return fs::symlink_metadata(path).map(|m| Meta::from_metadata(&m));
    #[cfg(windows)]
    return dfdu_sys::stat_path(path, false).map(|w| Meta::from_windows(&w));
}

/// Picks staging paths and remembers the staging directory found for each device.
pub(crate) struct Stager {
    session: String,
    /// `stage/` beside the journal directory.
    base: Option<PathBuf>,
    dirs: HashMap<u64, PathBuf>,
    n: u64,
}

impl Stager {
    pub fn new(session: String, base: Option<PathBuf>) -> Self {
        Stager {
            session,
            base,
            dirs: HashMap::new(),
            n: 0,
        }
    }

    /// Where `item` is to be staged: a new name in a staging directory on its filesystem,
    /// created if need be.
    pub fn target(&mut self, item: &PlanItem, scan_root: &Path) -> io::Result<PathBuf> {
        let dev = item.identity.dev;
        let path = item.path.to_local();
        let dir = if let Some(d) = self.dirs.get(&dev) {
            d.clone()
        } else {
            let d = self.find_dir(&path, dev, scan_root)?;
            self.dirs.insert(dev, d.clone());
            d
        };
        self.n += 1;
        let mut name = OsString::from(format!("{}-", self.n));
        name.push(path.file_name().unwrap_or_default());
        Ok(dir.join(name))
    }

    fn find_dir(&self, path: &Path, dev: u64, scan_root: &Path) -> io::Result<PathBuf> {
        let mut candidates = vec![];
        if let Some(b) = &self.base {
            candidates.push(b.join(&self.session));
        }
        candidates.push(scan_root.join(".dfdu-stage").join(&self.session));
        if let Some(p) = path.parent() {
            candidates.push(p.join(".dfdu-stage").join(&self.session));
        }
        for c in candidates {
            if c.starts_with(path) {
                continue;
            }
            // Only where the nearest existing ancestor is on the item's device, so nothing is
            // created on another filesystem.
            let near = c.ancestors().find(|a| fs::symlink_metadata(a).is_ok());
            if near.and_then(|a| meta(a).ok()).is_none_or(|m| m.dev != dev) {
                continue;
            }
            if fs::create_dir_all(&c).is_ok() && meta(&c).is_ok_and(|m| m.dev == dev) {
                return Ok(c);
            }
        }
        Err(io::Error::other(
            "no staging directory can be made on the item's filesystem",
        ))
    }
}

/// Puts `from` back at `original`, or at `<name>.restored-<n>` next to it if something is
/// there now. Returns where it went.
pub(crate) fn put_back(from: &Path, original: &Path) -> io::Result<PathBuf> {
    let mut to = original.to_path_buf();
    for n in 1..1000 {
        match rename_noreplace(from, &to) {
            Ok(()) => return Ok(to),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let mut name = original.file_name().unwrap_or_default().to_os_string();
                name.push(format!(".restored-{n}"));
                to = original.with_file_name(name);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Takes the item that was at `path` with identity `id` back out of the trash, where it went
/// at `trashed_at` (seconds since the epoch) or later.
pub(crate) fn restore_from_trash(
    path: &Path,
    id: &Identity,
    trashed_at: i64,
) -> io::Result<PathBuf> {
    imp::restore(path, id, trashed_at)
}

/// Whether that item is in the trash; `Err` when the trash cannot be read.
pub(crate) fn in_trash(path: &Path, id: &Identity, trashed_at: i64) -> io::Result<bool> {
    imp::find(path, id, trashed_at).map(|f| f.is_some())
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    use crate::Identity;

    /// The freedesktop trash and the Recycle Bin keep each item's original path and deletion
    /// time; the latest match is the item.
    pub fn find(
        path: &Path,
        _id: &Identity,
        trashed_at: i64,
    ) -> io::Result<Option<trash::TrashItem>> {
        Ok(trash::os_limited::list()
            .map_err(|e| io::Error::other(e.to_string()))?
            .into_iter()
            .filter(|t| t.original_path() == path && t.time_deleted >= trashed_at - 1)
            .max_by_key(|t| t.time_deleted))
    }

    /// The `trash` crate refuses to replace anything.
    pub fn restore(path: &Path, id: &Identity, trashed_at: i64) -> io::Result<PathBuf> {
        let found = find(path, id, trashed_at)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not found in the trash"))?;
        match trash::os_limited::restore_all([found]) {
            Ok(()) => Ok(path.to_path_buf()),
            Err(trash::Error::RestoreCollision { .. }) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "something now exists at {}; restore it from the trash by hand",
                    path.display()
                ),
            )),
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use crate::Identity;

    pub fn restore(path: &Path, id: &Identity, trashed_at: i64) -> io::Result<PathBuf> {
        let found = find(path, id, trashed_at)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not found in the Trash"))?;
        super::put_back(&found, path)
    }

    /// The macOS trash keeps no record of where an item came from that the system exposes, but
    /// moving to it is a rename on the item's volume (`~/.Trash`, or `.Trashes/<uid>` at the
    /// volume's root), so the item keeps its inode: it is found by `(dev, ino)`, which is never
    /// another file's.
    pub fn find(path: &Path, id: &Identity, _trashed_at: i64) -> io::Result<Option<PathBuf>> {
        let mut dirs = vec![];
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(Path::new(&h).join(".Trash"));
        }
        if let Some(root) = volume_root(path, id.dev) {
            dirs.push(
                root.join(".Trashes")
                    .join(rustix::process::getuid().as_raw().to_string()),
            );
        }
        let mut denied = false;
        for d in dirs {
            let rd = match fs::read_dir(&d) {
                Ok(rd) => rd,
                Err(e) => {
                    denied |= e.kind() == io::ErrorKind::PermissionDenied;
                    continue;
                }
            };
            for e in rd.flatten() {
                let p = e.path();
                if fs::symlink_metadata(&p).is_ok_and(|m| m.dev() == id.dev && m.ino() == id.ino) {
                    return Ok(Some(p));
                }
            }
        }
        if denied {
            // Reading the Trash needs Full Disk Access for the terminal.
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the Trash cannot be read here; use Put Back in the Finder",
            ))
        } else {
            Ok(None)
        }
    }

    /// The highest existing ancestor of `path` on device `dev`.
    fn volume_root(path: &Path, dev: u64) -> Option<PathBuf> {
        let mut root = None;
        for a in path.ancestors().skip(1) {
            match fs::symlink_metadata(a) {
                Ok(m) if m.dev() == dev => root = Some(a.to_path_buf()),
                Ok(_) => break,
                Err(_) => {}
            }
        }
        root
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    use crate::Identity;

    pub fn find(_path: &Path, _id: &Identity, _trashed_at: i64) -> io::Result<Option<()>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the trash cannot be read here",
        ))
    }

    pub fn restore(_path: &Path, _id: &Identity, _trashed_at: i64) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "undo from the trash is not supported here",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_noreplace_refuses_to_replace() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let e = rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"b");
        fs::remove_file(&b).unwrap();
        rename_noreplace(&a, &b).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"a");
    }

    #[test]
    fn put_back_goes_next_to_what_is_there_now() {
        let tmp = tempfile::tempdir().unwrap();
        let orig = tmp.path().join("x");
        let staged = tmp.path().join("staged");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("f"), b"old").unwrap();
        fs::create_dir(&orig).unwrap();
        fs::write(orig.join("f"), b"new").unwrap();
        let to = put_back(&staged, &orig).unwrap();
        assert_eq!(to, tmp.path().join("x.restored-1"));
        assert_eq!(fs::read(orig.join("f")).unwrap(), b"new");
        assert_eq!(fs::read(to.join("f")).unwrap(), b"old");
    }
}
