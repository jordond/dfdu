use std::fs;
use std::io;
use std::path::{Component, Path};

use dfdu_tree::{ErrKind, Kind, Meta};

use crate::{DeleteResult, Mode, PlanItem, Refusal};

/// Paths never deleted, whatever a plan or rule says (a minimal set): filesystem roots,
/// top-level system directories, the kernel filesystems, the user's home directory itself.
pub fn is_protected(path: &Path) -> bool {
    let normal = path
        .components()
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

/// Linux: the freedesktop home trash is on the filesystem of `$XDG_DATA_HOME`. The `trash`
/// crate copies items from other filesystems into it, which can fill the home filesystem and
/// takes as long as a copy, so such items are refused for trash mode.
/// On btrfs a different `st_dev` on the same mount is another subvolume.
#[cfg(all(unix, not(target_os = "macos")))]
fn trash_refusal(path: &Path, item_dev: u64) -> Option<Refusal> {
    use etcetera::BaseStrategy;
    use std::os::unix::fs::MetadataExt;
    let data = etcetera::base_strategy::Xdg::new()
        .ok()
        .map(|x| x.data_dir());
    let Some((dir, m)) = data.and_then(|d| fs::metadata(&d).ok().map(|m| (d, m))) else {
        return Some(Refusal::TrashCrossesFilesystems);
    };
    if m.dev() == item_dev {
        return None;
    }
    if same_mount(path, &dir) == Some(true) {
        Some(Refusal::TrashOtherSubvolume(subvolume_root(path, item_dev)))
    } else {
        Some(Refusal::TrashCrossesFilesystems)
    }
}

/// The root of the btrfs subvolume holding `path`: its highest ancestor on the same device
/// (every subvolume has its own `st_dev`).
#[cfg(all(unix, not(target_os = "macos")))]
fn subvolume_root(path: &Path, dev: u64) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let mut root = path;
    while let Some(p) = root.parent()
        && fs::symlink_metadata(p).is_ok_and(|m| m.dev() == dev)
    {
        root = p;
    }
    root.to_path_buf()
}

/// The uuid of the btrfs subvolume holding `path` (on device `dev`), in hex; `None` elsewhere.
#[cfg(target_os = "linux")]
pub(crate) fn subvolume_uuid(path: &Path, dev: u64) -> Option<String> {
    let info = dfdu_sys::subvol_info(&subvolume_root(path, dev)).ok()??;
    Some(hex::encode(info.uuid))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn subvolume_uuid(_path: &Path, _dev: u64) -> Option<String> {
    None
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn trash_refusal(_path: &Path, _item_dev: u64) -> Option<Refusal> {
    None
}

/// Whether two paths are on the same mount, when the system can tell (Linux mount ids).
fn same_mount(a: &Path, b: &Path) -> Option<bool> {
    #[cfg(target_os = "linux")]
    if let (Ok(Some(x)), Ok(Some(y))) = (dfdu_sys::mount_id(a), dfdu_sys::mount_id(b)) {
        return Some(x == y);
    }
    let _ = (a, b);
    None
}

/// Checks an item before deletion. `Ok` means it may be deleted now.
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
    // The same conversion the scanner used, so the fields compare like for like (on Windows
    // the file id and volume serial number, which std does not report).
    #[cfg(not(windows))]
    let now = fs::symlink_metadata(path).map(|m| Meta::from_metadata(&m));
    #[cfg(windows)]
    let now = dfdu_sys::stat_path(path, false).map(|w| Meta::from_windows(&w));
    let now = match now {
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
    let kind = now.kind;
    let same = kind == id.kind
        && now.dev == id.dev
        && now.ino == id.ino
        && now.mtime == id.mtime
        && (kind == Kind::Dir || now.apparent == id.apparent);
    if !same {
        return Err(DeleteResult::Stale("changed since the scan".into()));
    }
    // A mount point, judged by mount id where the system has one: btrfs subvolume roots have
    // their own `st_dev` but are on their parent's mount, and may be deleted.
    if kind == Kind::Dir
        && cfg!(unix)
        && let Some(parent) = path.parent()
        && let Ok(pm) = fs::symlink_metadata(parent)
    {
        let mount_point = match same_mount(path, parent) {
            Some(same) => !same,
            None => Meta::from_metadata(&pm).dev != now.dev,
        };
        if mount_point {
            return Err(DeleteResult::Refused(Refusal::MountPoint));
        }
    }
    if mode == Mode::Trash
        && let Some(r) = trash_refusal(path, now.dev)
    {
        return Err(DeleteResult::Refused(r));
    }
    Ok(())
}
