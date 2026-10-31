//! Permanent deletion that never leaves the item's filesystem.
//!
//! `std::fs::remove_dir_all` descends into whatever is mounted below the directory, so a mount
//! made after the scan would have its files deleted too. This walk opens each directory
//! relative to its parent without following symlinks, checks that it is still on the item's
//! mount before reading it, and stops with an error at the first that is not. btrfs subvolumes
//! are on the same mount; they are removed with the destroy ioctl when allowed (instant), else
//! emptied and removed with `rmdir`.

use std::io;
use std::path::Path;

use dfdu_tree::Kind;

/// Deletes `path` (a directory and everything in it, or a single entry) only if what it names
/// is still the device and inode `(dev, ino)`, checked on the item as opened for the delete, so
/// a path that was swapped since it was verified (a directory above it replaced by a symlink or
/// junction) deletes nothing.
///
/// # Errors
///
/// `path` is no longer `(dev, ino)`, something could not be deleted, or a directory below
/// `path` is on another mount (nothing below it is deleted then).
pub fn remove_verified(path: &Path, kind: Kind, dev: u64, ino: u64) -> io::Result<()> {
    imp::remove(path, kind, Some((dev, ino)))
}

#[cfg(unix)]
mod imp {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use dfdu_tree::Kind;
    use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};

    fn errno(e: rustix::io::Errno) -> io::Error {
        io::Error::from_raw_os_error(e.raw_os_error())
    }

    fn open_dir(dir: impl AsFd, name: &OsStr) -> io::Result<OwnedFd> {
        rustix::fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)
    }

    /// What identifies the filesystem a directory is on: the mount id where the system has
    /// one (btrfs subvolumes share it), else the device.
    #[derive(Copy, Clone, PartialEq, Eq)]
    struct Fs {
        mount: Option<u64>,
        dev: u64,
    }

    impl Fs {
        fn same(self, o: Fs) -> bool {
            match (self.mount, o.mount) {
                (Some(a), Some(b)) => a == b,
                _ => self.dev == o.dev,
            }
        }
    }

    /// The filesystem and inode number of an open directory.
    fn fs_of(dir: BorrowedFd<'_>) -> io::Result<(Fs, u64)> {
        let st = rustix::fs::fstat(dir).map_err(errno)?;
        #[cfg(target_os = "linux")]
        let mount = dfdu_sys::mount_id_fd(dir)?;
        #[cfg(not(target_os = "linux"))]
        let mount = None;
        Ok((
            Fs {
                mount,
                dev: dev_of(&st),
            },
            st.st_ino as u64,
        ))
    }

    /// The device in `st`, widened as std's `MetadataExt::dev` widens it: `dev_t` is signed on
    /// macOS, and sign-extended.
    #[cfg(target_os = "macos")]
    fn dev_of(st: &rustix::fs::Stat) -> u64 {
        i64::from(st.st_dev).cast_unsigned()
    }

    #[cfg(not(target_os = "macos"))]
    fn dev_of(st: &rustix::fs::Stat) -> u64 {
        st.st_dev
    }

    fn changed(path: &Path) -> io::Error {
        io::Error::other(format!(
            "{} is no longer the item that was checked; not deleting it",
            path.display()
        ))
    }

    pub fn remove(path: &Path, kind: Kind, expect: Option<(u64, u64)>) -> io::Result<()> {
        let (parent, name) = match (path.parent(), path.file_name()) {
            (Some(p), Some(n)) if !p.as_os_str().is_empty() => (p, n),
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        };
        let pfd = open_dir(CWD, parent.as_os_str())?;
        if kind != Kind::Dir {
            if let Some((dev, ino)) = expect {
                let st =
                    rustix::fs::statat(&pfd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
                if (dev_of(&st), st.st_ino as u64) != (dev, ino) {
                    return Err(changed(path));
                }
            }
            return rustix::fs::unlinkat(&pfd, name, AtFlags::empty()).map_err(errno);
        }
        let dfd = open_dir(&pfd, name)?;
        if let Some((dev, ino)) = expect {
            let st = rustix::fs::fstat(&dfd).map_err(errno)?;
            if (dev_of(&st), st.st_ino as u64) != (dev, ino) {
                return Err(changed(path));
            }
        }
        let (root, _) = fs_of(dfd.as_fd())?;
        empty_and_remove(pfd.as_fd(), name, dfd, root, path)
    }

    fn remove_dir(parent: BorrowedFd<'_>, name: &OsStr, root: Fs, path: &Path) -> io::Result<()> {
        let dfd = open_dir(parent, name)?;
        empty_and_remove(parent, name, dfd, root, path)
    }

    /// Empties the directory open as `dfd` (`name` in `parent`) and removes it.
    fn empty_and_remove(
        parent: BorrowedFd<'_>,
        name: &OsStr,
        dfd: OwnedFd,
        root: Fs,
        path: &Path,
    ) -> io::Result<()> {
        // Checked on the opened directory itself, so a mount made in between is seen.
        let (here, ino) = fs_of(dfd.as_fd())?;
        if !here.same(root) {
            return Err(io::Error::other(format!(
                "a filesystem is mounted at {}; not deleting into it",
                path.display()
            )));
        }
        #[cfg(target_os = "linux")]
        if ino == dfdu_sys::BTRFS_SUBVOL_ROOT_INO && dfdu_sys::is_btrfs_fd(&dfd)? {
            // Root, or `user_subvol_rm_allowed` for the owner. Otherwise (or with nested
            // subvolumes inside) the subvolume is emptied below and removed with rmdir.
            if dfdu_sys::destroy_subvolume_at(parent, name).is_ok() {
                return Ok(());
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = ino;
        // Collected first: the directory is not modified while it is being read.
        let mut entries: Vec<(OsString, FileType)> = Vec::new();
        for e in Dir::read_from(&dfd).map_err(errno)? {
            let e = e.map_err(errno)?;
            let n = e.file_name().to_bytes();
            if n == b"." || n == b".." {
                continue;
            }
            entries.push((OsStr::from_bytes(n).to_owned(), e.file_type()));
        }
        for (n, ft) in entries {
            let is_dir = match ft {
                FileType::Directory => true,
                FileType::Unknown => {
                    let st = rustix::fs::statat(&dfd, n.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(errno)?;
                    FileType::from_raw_mode(st.st_mode) == FileType::Directory
                }
                _ => false,
            };
            if is_dir {
                remove_dir(dfd.as_fd(), &n, root, &path.join(&n))?;
            } else {
                rustix::fs::unlinkat(&dfd, n.as_os_str(), AtFlags::empty()).map_err(errno)?;
            }
        }
        drop(dfd);
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(errno)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    use dfdu_tree::Kind;

    /// Windows: `remove_dir_all` does not follow junctions or mounted-folder reparse points; it
    /// removes the link itself. With `expect`, the item is opened first and its volume serial
    /// number and file id compared, then its contents are removed while that handle keeps it
    /// and the directories above it from being renamed, and the item is deleted through the
    /// handle.
    pub fn remove(path: &Path, kind: Kind, expect: Option<(u64, u64)>) -> io::Result<()> {
        use std::os::windows::io::AsHandle;
        let Some((dev, ino)) = expect else {
            return if kind == Kind::Dir {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
        };
        let h = dfdu_sys::open_for_delete(path)?;
        let m = dfdu_tree::Meta::from_windows(&dfdu_sys::stat_open(h.as_handle())?);
        if (m.dev, m.ino) != (dev, ino) {
            return Err(io::Error::other(format!(
                "{} is no longer the item that was checked; not deleting it",
                path.display()
            )));
        }
        // A directory itself, not a junction or symlink to one (those are removed as links).
        if m.kind == Kind::Dir {
            use std::os::windows::fs::FileTypeExt;
            for e in std::fs::read_dir(path)? {
                let (e, t) = e.and_then(|e| Ok((e.path(), e.file_type()?)))?;
                if t.is_dir() {
                    std::fs::remove_dir_all(e)?;
                } else if t.is_symlink_dir() {
                    // A junction or a directory symlink: the link only.
                    std::fs::remove_dir(e)?;
                } else {
                    std::fs::remove_file(e)?;
                }
            }
        }
        dfdu_sys::delete_open(h.as_handle())
    }
}

#[cfg(test)]
mod tests;
