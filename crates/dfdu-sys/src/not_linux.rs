//! The Linux-only calls on every other system: there are no mount ids, subvolumes or btrfs
//! there, so each answers "none" without asking the OS.

use std::io;
use std::path::Path;

use crate::SubvolInfo;

/// Mount ids are a Linux concept; elsewhere `st_dev` identifies the filesystem.
///
/// # Errors
///
/// None here; the Linux version reports `statx` failures.
pub fn mount_id(_path: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

/// Subvolumes are a btrfs concept, so there are none here.
///
/// # Errors
///
/// None here; the Linux version reports `ioctl` failures.
pub fn subvol_info(_path: &Path) -> io::Result<Option<SubvolInfo>> {
    Ok(None)
}

/// btrfs is Linux-only.
///
/// # Errors
///
/// None here; the Linux version reports `statfs` failures.
pub fn is_btrfs(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

/// The mount table is read from `/proc` on Linux only.
///
/// # Errors
///
/// Always `Unsupported`.
pub fn mount_table() -> io::Result<Vec<crate::MountEntry>> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Whether files on the filesystem at `path` can share data; not known here.
///
/// # Errors
///
/// None here; the Linux and macOS versions report `statfs` failures.
#[cfg(not(target_os = "macos"))]
pub fn shares_extents(_path: &Path) -> io::Result<bool> {
    Ok(false)
}
