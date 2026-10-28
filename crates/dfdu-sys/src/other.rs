use std::io;
use std::path::Path;

use crate::SubvolInfo;

/// Mount ids are a Linux concept; elsewhere `st_dev` identifies the filesystem.
pub fn mount_id(_path: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

pub fn subvol_info(_path: &Path) -> io::Result<Option<SubvolInfo>> {
    Ok(None)
}

pub fn is_btrfs(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

pub fn mount_table() -> io::Result<Vec<crate::MountEntry>> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(target_os = "macos"))]
pub fn shares_extents(_path: &Path) -> io::Result<bool> {
    Ok(false)
}
