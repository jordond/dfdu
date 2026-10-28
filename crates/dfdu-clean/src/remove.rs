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

/// Deletes `path` (a directory and everything in it, or a single entry).
pub fn remove(path: &Path, kind: Kind) -> io::Result<()> {
    imp::remove(path, kind, None)
}

/// Deletes `path` only if what it names is still the device and inode `(dev, ino)`, checked
/// on the item as opened for the delete, so a path that was swapped since it was verified (a
/// directory above it replaced by a symlink) deletes nothing. Windows checks nothing more than
/// [`remove`].
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
                dev: st.st_dev as u64,
            },
            st.st_ino as u64,
        ))
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
                if (st.st_dev as u64, st.st_ino as u64) != (dev, ino) {
                    return Err(changed(path));
                }
            }
            return rustix::fs::unlinkat(&pfd, name, AtFlags::empty()).map_err(errno);
        }
        let dfd = open_dir(&pfd, name)?;
        if let Some((dev, ino)) = expect {
            let st = rustix::fs::fstat(&dfd).map_err(errno)?;
            if (st.st_dev as u64, st.st_ino as u64) != (dev, ino) {
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
    /// removes the link itself.
    pub fn remove(path: &Path, kind: Kind, _expect: Option<(u64, u64)>) -> io::Result<()> {
        if kind == Kind::Dir {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn removes_trees_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), b"k").unwrap();
        let d = tmp.path().join("d");
        std::fs::create_dir_all(d.join("a/b/c")).unwrap();
        std::fs::write(d.join("a/b/c/f"), b"x").unwrap();
        std::fs::write(d.join("top"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.join("a/link")).unwrap();
        remove(&d, Kind::Dir).unwrap();
        assert!(!d.exists());
        assert!(outside.path().join("keep").exists(), "followed a symlink");
        std::fs::write(tmp.path().join("f"), b"x").unwrap();
        remove(&tmp.path().join("f"), Kind::File).unwrap();
        assert!(!tmp.path().join("f").exists());
    }

    /// A directory above the item replaced by a symlink after the item was checked: the path
    /// now names another directory, which is not deleted. (A symlink as the parent itself is
    /// refused when the parent is opened.)
    #[test]
    fn a_swapped_path_deletes_nothing() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let item = tmp.path().join("a/b/item");
        std::fs::create_dir_all(&item).unwrap();
        std::fs::write(tmp.path().join("a/b/file"), b"x").unwrap();
        let m = std::fs::symlink_metadata(&item).unwrap();
        let f = std::fs::symlink_metadata(tmp.path().join("a/b/file")).unwrap();
        let other = tmp.path().join("other");
        std::fs::create_dir_all(other.join("b/item")).unwrap();
        std::fs::write(other.join("b/item/precious"), b"p").unwrap();
        std::fs::write(other.join("b/file"), b"p").unwrap();
        std::fs::rename(tmp.path().join("a"), tmp.path().join("a-moved")).unwrap();
        std::os::unix::fs::symlink(&other, tmp.path().join("a")).unwrap();
        let e = remove_verified(&item, Kind::Dir, m.dev(), m.ino()).unwrap_err();
        assert!(e.to_string().contains("no longer the item"), "{e}");
        assert!(other.join("b/item/precious").exists());
        let file = tmp.path().join("a/b/file");
        let e = remove_verified(&file, Kind::File, f.dev(), f.ino()).unwrap_err();
        assert!(e.to_string().contains("no longer the item"), "{e}");
        assert!(other.join("b/file").exists());
        // Where they are now, they are deleted.
        let moved = tmp.path().join("a-moved/b");
        remove_verified(&moved.join("item"), Kind::Dir, m.dev(), m.ino()).unwrap();
        remove_verified(&moved.join("file"), Kind::File, f.dev(), f.ino()).unwrap();
        assert!(!moved.join("item").exists() && !moved.join("file").exists());
    }

    #[test]
    fn missing_item_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = remove(&tmp.path().join("nope"), Kind::Dir).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    /// Root only (mounts a tmpfs): a mount inside the item stops the delete there.
    #[cfg(target_os = "linux")]
    #[test]
    fn stops_at_a_mount() {
        if !dfdu_sys::is_privileged() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("d");
        std::fs::create_dir_all(d.join("m")).unwrap();
        std::fs::write(d.join("before"), b"x").unwrap();
        let mounted = std::process::Command::new("mount")
            .args(["-t", "tmpfs", "tmpfs"])
            .arg(d.join("m"))
            .status()
            .is_ok_and(|s| s.success());
        if !mounted {
            return;
        }
        std::fs::write(d.join("m/precious"), b"p").unwrap();
        let e = remove(&d, Kind::Dir).unwrap_err();
        let still = d.join("m/precious").exists();
        let _ = std::process::Command::new("umount")
            .arg(d.join("m"))
            .status();
        assert!(e.to_string().contains("mounted"), "{e}");
        assert!(still, "deleted into a mount");
    }
}
