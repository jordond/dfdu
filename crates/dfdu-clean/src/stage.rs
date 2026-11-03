//! Staging, and putting items back. A staged item is renamed into this session's staging
//! directory on its own filesystem: `stage/<session>/` beside the
//! journals when that is on the item's filesystem, else `.dfdu-stage/<session>/` in the scan
//! root or, failing that, next to the item. Every rename that puts something back refuses to
//! replace what is at the target.
//!
//! On Unix a staging move goes by file descriptor, as a permanent delete does
//! (`crate::remove`), because someone who can write a directory above the item (`/tmp`,
//! `/Users/Shared`) could swap a component for a symlink between the check and the move: the
//! item's directory is opened without following a symlink and the item is checked there to
//! still be the one verified; the staging directories are made and opened the same way, each
//! relative to the one before, and refused if what is there is a symlink, not a directory, on
//! another filesystem or owned by someone else; the rename is between the two descriptors.

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

/// The metadata of `path` itself, not of what a link at it points to.
pub(crate) fn meta(path: &Path) -> io::Result<Meta> {
    #[cfg(not(windows))]
    return fs::symlink_metadata(path).map(|m| Meta::from_metadata(&m));
    #[cfg(windows)]
    return dfdu_sys::stat_path(path, false).map(|w| Meta::from_windows(&w));
}

/// A staging directory found for one device.
struct StageDir {
    path: PathBuf,
    /// The directory itself, opened when it was made and checked: items are renamed into it by
    /// this descriptor, so its path changed since then is not followed.
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

/// Picks staging paths and remembers the staging directory found for each device.
pub(crate) struct Stager {
    session: String,
    /// `stage/` beside the journal directory.
    base: Option<PathBuf>,
    dirs: HashMap<u64, StageDir>,
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
        if !self.dirs.contains_key(&dev) {
            let d = self.find_dir(&path, dev, scan_root)?;
            self.dirs.insert(dev, d);
        }
        let dir = self
            .dirs
            .get(&dev)
            .map_or(Path::new(""), |d| d.path.as_path());
        self.n += 1;
        let mut name = OsString::from(format!("{}-", self.n));
        name.push(path.file_name().unwrap_or_default());
        Ok(dir.join(name))
    }

    /// Stages the verified item at `from`, with identity `id`: renames it to `to`, a path
    /// [`Stager::target`] gave for it, refusing to replace anything there. On Unix, only if
    /// `from` still is `id` when its directory is opened.
    pub fn stage(&self, from: &Path, id: &Identity, to: &Path) -> io::Result<()> {
        let dir = self
            .dirs
            .get(&id.dev)
            .filter(|d| to.parent() == Some(d.path.as_path()))
            .ok_or_else(|| io::Error::other("not a staging path of this session"))?;
        #[cfg(unix)]
        {
            let name = to.file_name().ok_or(io::ErrorKind::InvalidInput)?;
            at::move_into(from, id, &dir.fd, name)
        }
        #[cfg(not(unix))]
        {
            let _ = (dir, id);
            rename_noreplace(from, to)
        }
    }

    /// The first candidate that can be made: each staging directory is made and opened relative
    /// to the directory it is in, without following a symlink.
    #[cfg(unix)]
    fn find_dir(&self, path: &Path, dev: u64, scan_root: &Path) -> io::Result<StageDir> {
        use std::ffi::OsStr;
        let stage = OsStr::new(".dfdu-stage");
        // (the directory it is made in, its name there, whether the user dfdu acts for may own
        // it: the state directory's `stage`, which is given back to them)
        let mut candidates: Vec<(&Path, &OsStr, bool)> = vec![];
        if let Some(b) = &self.base
            && let (Some(p), Some(n)) = (b.parent(), b.file_name())
        {
            candidates.push((p, n, true));
        }
        candidates.push((scan_root, stage, false));
        if let Some(p) = path.parent() {
            candidates.push((p, stage, false));
        }
        for (within, name, theirs) in candidates {
            let c = within.join(name).join(&self.session);
            if c.starts_with(path) {
                continue;
            }
            let session = OsStr::new(&self.session);
            if let Ok(fd) = at::open_stage(within, name, theirs, session, dev) {
                return Ok(StageDir { path: c, fd });
            }
        }
        Err(io::Error::other(
            "no staging directory can be made on the item's filesystem",
        ))
    }

    #[cfg(not(unix))]
    fn find_dir(&self, path: &Path, dev: u64, scan_root: &Path) -> io::Result<StageDir> {
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
                return Ok(StageDir { path: c });
            }
        }
        Err(io::Error::other(
            "no staging directory can be made on the item's filesystem",
        ))
    }
}

/// Staging by file descriptor (see the module's comment).
#[cfg(unix)]
mod at {
    use std::ffi::OsStr;
    use std::io;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{AtFlags, CWD, Mode};

    use crate::Identity;
    use crate::remove::{dev_of, errno, open_dir};

    /// Makes (or finds) `within/name/session` and opens it. `name` may be owned by the user dfdu
    /// acts for when `theirs`; otherwise both must be this process's user's.
    pub fn open_stage(
        within: &Path,
        name: &OsStr,
        theirs: bool,
        session: &OsStr,
        dev: u64,
    ) -> io::Result<OwnedFd> {
        let wfd = open_dir(CWD, within.as_os_str())?;
        // Nothing is made on another filesystem.
        if dev_of(&rustix::fs::fstat(&wfd).map_err(errno)?) != dev {
            return Err(io::Error::other("on another filesystem"));
        }
        let sfd = make_dir(&wfd, name, dev, theirs)?;
        make_dir(&sfd, session, dev, false)
    }

    /// Makes the directory `name` in `parent` unless it is there, and opens it without
    /// following a symlink. Refused unless it is on `dev` and owned by this process's user (or,
    /// when `theirs`, by the user dfdu acts for): a directory someone else made (or a symlink
    /// they planted) is not staged into, and a root dfdu stages only into root's.
    fn make_dir(parent: impl AsFd, name: &OsStr, dev: u64, theirs: bool) -> io::Result<OwnedFd> {
        match rustix::fs::mkdirat(&parent, name, Mode::RWXU | Mode::RWXG | Mode::RWXO) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(errno(e)),
        }
        let fd = open_dir(&parent, name)?;
        let st = rustix::fs::fstat(&fd).map_err(errno)?;
        if dev_of(&st) != dev || !owned(st.st_uid, theirs) {
            return Err(io::Error::other(format!(
                "{} is not a staging directory dfdu made",
                name.display()
            )));
        }
        Ok(fd)
    }

    /// Whether `uid` is this process's user, or with `theirs` the user dfdu acts for.
    fn owned(uid: u32, theirs: bool) -> bool {
        uid == rustix::process::geteuid().as_raw()
            || (theirs && uid == dfdu_sys::acting::owner_uid())
    }

    /// The directory `path` is in, and its name there.
    fn split(path: &Path) -> io::Result<(&Path, &OsStr)> {
        match (path.parent(), path.file_name()) {
            (Some(p), Some(n)) if !p.as_os_str().is_empty() => Ok((p, n)),
            _ => Err(io::ErrorKind::InvalidInput.into()),
        }
    }

    /// Whether `name` in `dir` is `id`.
    fn is(dir: &OwnedFd, name: &OsStr, id: &Identity) -> io::Result<bool> {
        let st = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
        Ok((dev_of(&st), st.st_ino as u64) == (id.dev, id.ino))
    }

    /// Renames the item at `from` to `name` in `dir` if it still is `id`, checked in its
    /// directory as opened for the rename.
    pub fn move_into(from: &Path, id: &Identity, dir: &OwnedFd, name: &OsStr) -> io::Result<()> {
        let (parent, item) = split(from)?;
        let pfd = open_dir(CWD, parent.as_os_str())?;
        if !is(&pfd, item, id)? {
            return Err(io::Error::other(format!(
                "{} is no longer the item that was checked; not staging it",
                from.display()
            )));
        }
        rename(&pfd, item, dir, name)
    }

    /// Whether a root dfdu trusts a staging directory owned by `owner` to hold what it staged:
    /// only its own, as the user can replace a directory of theirs (or rename root's away from
    /// `stage/`, which is theirs, and put their own in its place). Any process else can only
    /// move what it may move anyway.
    pub(super) fn trusted(euid: u32, owner: u32) -> bool {
        euid != 0 || owner == 0
    }

    /// [`super::put_back`] by file descriptor: the directory `from` is in is opened without
    /// following a symlink and, when `staged`, checked to be one a root dfdu trusts; `from` is
    /// checked there to still be `id`; the directory `original` is in is opened the same way
    /// and the item renamed between the two, never replacing anything.
    pub fn move_back(
        from: &Path,
        id: &Identity,
        original: &Path,
        staged: bool,
    ) -> io::Result<std::path::PathBuf> {
        let (dir, item) = split(from)?;
        let sfd = open_dir(CWD, dir.as_os_str())?;
        if staged {
            let st = rustix::fs::fstat(&sfd).map_err(errno)?;
            if !trusted(rustix::process::geteuid().as_raw(), st.st_uid) {
                return Err(io::Error::other(format!(
                    "{} is not root's, so it may not be where the item was staged; not putting \
                     it back",
                    dir.display()
                )));
            }
        }
        if !is(&sfd, item, id)? {
            return Err(io::Error::other(format!(
                "{} is no longer the item that was staged; not putting it back",
                from.display()
            )));
        }
        let (parent, _) = split(original)?;
        let pfd = open_dir(CWD, parent.as_os_str())?;
        super::first_free(original, |to| {
            rename(&sfd, item, &pfd, to.file_name().unwrap_or_default())
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rename(from: &OwnedFd, old: &OsStr, to: &OwnedFd, new: &OsStr) -> io::Result<()> {
        use rustix::fs::{RenameFlags, renameat_with};
        renameat_with(from, old, to, new, RenameFlags::NOREPLACE).map_err(errno)
    }

    /// No atomic form here: the target is checked first (a narrow race), as
    /// [`super::rename_noreplace`] does.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn rename(from: &OwnedFd, old: &OsStr, to: &OwnedFd, new: &OsStr) -> io::Result<()> {
        if rustix::fs::statat(to, new, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        rustix::fs::renameat(from, old, to, new).map_err(errno)
    }
}

/// Puts the staged item `from`, whose identity is `id` (a rename keeps it), back at
/// `original`, or at `<name>.restored-<n>` next to it if something is there now. Returns where
/// it went. On Unix the move goes by file descriptor and is refused, leaving everything in
/// place, when `from` is no longer `id` or, for a root dfdu, its staging directory is not
/// root's.
pub(crate) fn put_back(from: &Path, id: &Identity, original: &Path) -> io::Result<PathBuf> {
    move_back(from, id, original, true)
}

/// [`put_back`]; `staged` when `from` is in a staging directory (not the macOS Trash).
fn move_back(from: &Path, id: &Identity, original: &Path, staged: bool) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        at::move_back(from, id, original, staged)
    }
    #[cfg(not(unix))]
    {
        let _ = (id, staged);
        first_free(original, |to| rename_noreplace(from, to))
    }
}

/// Calls `rename` with `original`, then `<name>.restored-<n>` beside it, until one is not
/// there. Returns the one it took.
fn first_free(
    original: &Path,
    mut rename: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let mut to = original.to_path_buf();
    for n in 1..1000 {
        match rename(&to) {
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
        super::move_back(&found, id, path, false)
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
                    .join(dfdu_sys::acting::owner_uid().to_string()),
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
mod tests;
