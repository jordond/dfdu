use std::fs;
use std::io;
use std::path::{Component, Path};
use std::sync::LazyLock;

use dfdu_rules::protected::Protected;
use dfdu_rules::{Env, rule_path};

use dfdu_tree::{ErrKind, Kind, Meta};

use crate::{DeleteResult, Mode, PlanItem, Refusal};

/// Paths never deleted, whatever a plan or rule says: filesystem roots, top-level directories,
/// the user's home directory itself, and everything at or below a protected path or name of
/// `dfdu_rules::protected`, with its exceptions, so the tree
/// and this check agree on what may be deleted. Files tracked by version control are protected
/// by the classifier, in the tree.
#[must_use]
pub fn is_protected(path: &Path) -> bool {
    // One snapshot of the environment for the protected paths and the home directory.
    static RULES: LazyLock<(Protected, Option<String>)> = LazyLock::new(|| {
        let env = Env::current();
        (Protected::new(&env), env.home())
    });
    protected_by(&RULES.0, RULES.1.as_deref(), path)
}

/// [`is_protected`] for the protected paths `rules` and the home directory `home` (as
/// [`Env::home`] gives it).
fn protected_by(rules: &Protected, home: Option<&str>, path: &Path) -> bool {
    let normal = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if normal <= 1 {
        // "/", "/usr", "C:\", "C:\Windows" and the like.
        return true;
    }
    let p = rule_path(path);
    // The home directory is excepted from the protection of the directory holding it (so
    // what is below it can be deleted), but is never deleted itself.
    home.is_some_and(|h| p == h.as_bytes()) || rules.covers(&p)
}

/// Linux: the freedesktop home trash is on the filesystem of `$XDG_DATA_HOME`. The `trash`
/// crate copies items from other filesystems into it, which can fill the home filesystem and
/// takes as long as a copy, so such items are refused for trash mode.
/// On btrfs a different `st_dev` on the same mount is another subvolume. A data directory that
/// does not exist yet is created by the trash crate, on the filesystem of its nearest ancestor.
#[cfg(all(unix, not(target_os = "macos")))]
fn trash_refusal(path: &Path, item_dev: u64) -> Option<Refusal> {
    use etcetera::BaseStrategy;
    use std::os::unix::fs::MetadataExt;
    let data = etcetera::base_strategy::Xdg::new()
        .ok()
        .map(|x| x.data_dir());
    let found = data.and_then(|d| {
        d.ancestors()
            .find_map(|a| fs::metadata(a).ok().map(|m| (a.to_path_buf(), m)))
    });
    let Some((dir, m)) = found else {
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
#[cfg(target_os = "linux")]
fn same_mount(a: &Path, b: &Path) -> Option<bool> {
    match (dfdu_sys::mount_id(a), dfdu_sys::mount_id(b)) {
        (Ok(Some(x)), Ok(Some(y))) => Some(x == y),
        _ => None,
    }
}

#[cfg(not(target_os = "linux"))]
fn same_mount(_a: &Path, _b: &Path) -> Option<bool> {
    None
}

/// The first directory below `dir` (on device `dev`) that is a mount point, without following
/// symlinks. A directory that cannot be read is not entered: its own mount was checked.
fn mount_inside(dir: &Path, dev: u64) -> Option<std::path::PathBuf> {
    let mut stack = vec![(dir.to_path_buf(), dev)];
    while let Some((d, dev)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let p = e.path();
            let Ok(m) = fs::symlink_metadata(&p) else {
                continue;
            };
            let sub = Meta::from_metadata(&m).dev;
            // btrfs subvolumes have their own device on their parent's mount.
            if sub != dev && same_mount(&p, &d) != Some(true) {
                return Some(p);
            }
            stack.push((p, sub));
        }
    }
    None
}

/// Checks an item before deletion. `Ok` means it may be deleted now.
///
/// # Errors
///
/// The result to report instead of deleting: refused (outside the scan root, protected, on
/// another filesystem), gone, or changed since the scan.
pub fn verify(item: &PlanItem, scan_root: &Path, mode: Mode) -> Result<(), DeleteResult> {
    let path = &item.path;
    if !path.is_absolute() || !scan_root.is_absolute() {
        // Callers canonicalise the scan root; a relative path here is a bug, not a target.
        return Err(DeleteResult::Refused(Refusal::OutsideScanRoot));
    }
    // `..` would leave the root while the path still starts with it; plan paths are built
    // from names and never have one.
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
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
    // A directory above the item replaced by a symlink since the scan would take the delete
    // somewhere else, outside the root or into a protected path: the item's directory must
    // resolve to itself (scan roots are canonical).
    match path.parent().map(dunce::canonicalize) {
        Some(Ok(real)) if real == path.parent().unwrap_or(path) => {}
        Some(Ok(_)) => {
            return Err(DeleteResult::Stale(
                "a directory above it is now a symlink".into(),
            ));
        }
        Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
            return Err(DeleteResult::Stale("no longer exists".into()));
        }
        Some(Err(e)) => {
            return Err(DeleteResult::Failed {
                partial: false,
                err: ErrKind::from(&e),
                msg: e.to_string(),
            });
        }
        None => return Err(DeleteResult::Refused(Refusal::OutsideScanRoot)),
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
    // A rename takes whatever is mounted inside along with the item (a permanent delete
    // stops at the mount on its own).
    if kind == Kind::Dir
        && cfg!(unix)
        && matches!(mode, Mode::Trash | Mode::Stage)
        && let Some(m) = mount_inside(path, now.dev)
    {
        return Err(DeleteResult::Refused(Refusal::ContainsMount(m)));
    }
    if mode == Mode::Trash
        && let Some(r) = trash_refusal(path, now.dev)
    {
        return Err(DeleteResult::Refused(r));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfdu_rules::Platform;

    fn check(platform: Platform, home: &str) -> impl Fn(&str) -> bool {
        let env = Env::with(platform, &[("HOME", home)]);
        let rules = Protected::new(&env);
        let home = env.home();
        move |p| protected_by(&rules, home.as_deref(), Path::new(p))
    }

    /// Deletion refuses what the tree marks protected, with the same exceptions, and also
    /// top-level directories and the home directory itself.
    #[test]
    fn protected_paths_follow_the_rules_and_their_exceptions() {
        let mac = check(Platform::MacOs, "/Users/u");
        assert!(mac("/"));
        assert!(mac("/usr"));
        assert!(mac("/usr/bin/true"));
        assert!(mac("/System/Library/x"));
        assert!(mac("/dev/fd/3"));
        assert!(mac("/etc/hosts"));
        // `/usr/local` is excepted from `/usr` on macOS (Homebrew, global npm packages).
        assert!(!mac("/usr/local/lib/node_modules"));
        assert!(!mac("/usr/local/Cellar/x"));
        assert!(mac("/Users/u"));
        assert!(!mac("/Users/u/Library/Caches/pip"));
        assert!(mac("/Users/u/.ssh/config"));
        assert!(mac("/Users/other/Library/Caches"));

        let linux = check(Platform::Linux, "/home/u");
        assert!(linux("/usr/local/lib/node_modules"));
        assert!(linux("/proc/1/fd"));
        assert!(linux("/sys/fs/cgroup"));
        assert!(linux("/dev/shm/x"));
        assert!(linux("/etc/hosts"));
        assert!(linux("/boot/efi"));
        assert!(linux("/home/u"));
        assert!(!linux("/home/u/.cache/pip"));
        assert!(!linux("/var/cache/apt"));
    }
}
