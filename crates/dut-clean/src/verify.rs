use std::fs;
use std::io;
use std::path::{Component, Path};

use dut_tree::{ErrKind, Kind};

use crate::{DeleteResult, Mode, PlanItem, Refusal};

/// Paths never deleted, whatever a plan or rule says (`categories-and-cleanup.md` §8, minimal
/// M1 set): filesystem roots, top-level system directories, the kernel filesystems, the user's
/// home directory itself.
pub fn is_protected(path: &Path) -> bool {
    let comps: Vec<Component> = path.components().collect();
    let normal = comps
        .iter()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if normal <= 1 {
        // "/", "/usr", "C:\", "C:\Windows" and the like.
        return true;
    }
    for p in [
        "/proc", "/sys", "/dev", "/System", "/usr", "/bin", "/sbin", "/etc", "/boot",
    ] {
        if path.starts_with(p) {
            return true;
        }
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        && path == Path::new(&home)
    {
        return true;
    }
    false
}

#[cfg(unix)]
fn identity_of(m: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

#[cfg(windows)]
fn identity_of(m: &fs::Metadata) -> (u64, u64) {
    // Matches the portable backend: creation time in place of a file id (see meta_from_std).
    use std::os::windows::fs::MetadataExt;
    (0, m.creation_time())
}

#[cfg(not(any(unix, windows)))]
fn identity_of(_m: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn kind_of(m: &fs::Metadata) -> Kind {
    let ft = m.file_type();
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn mtime_of(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Linux: the freedesktop home trash is on the filesystem of `$XDG_DATA_HOME`. The `trash`
/// crate copies items from other filesystems into it, which can fill the home filesystem and
/// takes as long as a copy, so such items are refused for trash mode.
#[cfg(all(unix, not(target_os = "macos")))]
fn trash_crosses_filesystems(item_dev: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")));
    match data.and_then(|d| fs::metadata(d).ok()) {
        Some(m) => m.dev() != item_dev,
        None => true,
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn trash_crosses_filesystems(_item_dev: u64) -> bool {
    false
}

/// Checks an item before deletion (§7.4 step 2). `Ok` means it may be deleted now.
pub fn verify(item: &PlanItem, scan_root: &Path, mode: Mode) -> Result<(), DeleteResult> {
    let path = &item.path;
    if !path.is_absolute() || !scan_root.is_absolute() {
        // Callers canonicalise the scan root; a relative path here is a bug, not a target.
        return Err(DeleteResult::Refused(Refusal::OutsideScanRoot));
    }
    if path == scan_root {
        return Err(DeleteResult::Refused(Refusal::ScanRoot));
    }
    if !path.starts_with(scan_root) {
        return Err(DeleteResult::Refused(Refusal::OutsideScanRoot));
    }
    if is_protected(path) {
        return Err(DeleteResult::Refused(Refusal::Protected));
    }
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(DeleteResult::Stale("no longer exists".into()));
        }
        Err(e) => {
            return Err(DeleteResult::Failed {
                partial: false,
                err: ErrKind::from(&e),
                msg: e.to_string(),
            });
        }
    };
    let id = &item.identity;
    let (dev, ino) = identity_of(&m);
    let kind = kind_of(&m);
    let same = kind == id.kind
        && dev == id.dev
        && ino == id.ino
        && mtime_of(&m) == id.mtime
        && (kind == Kind::Dir || m.len() == id.apparent);
    if !same {
        return Err(DeleteResult::Stale("changed since the scan".into()));
    }
    if kind == Kind::Dir
        && let Some(parent) = path.parent().and_then(|p| fs::symlink_metadata(p).ok())
        && cfg!(unix)
        && identity_of(&parent).0 != dev
    {
        return Err(DeleteResult::Refused(Refusal::MountPoint));
    }
    if mode == Mode::Trash && cfg!(unix) && trash_crosses_filesystems(dev) {
        return Err(DeleteResult::Refused(Refusal::TrashCrossesFilesystems));
    }
    Ok(())
}
