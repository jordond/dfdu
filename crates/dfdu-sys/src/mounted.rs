//! Mounts a scan lists but does not enter (`-x`): their type from the mount table, and how much
//! of them is used. The type is read without touching the mount; the used space only of local
//! filesystems, each on its own thread with a deadline, since `statvfs` on a dead network mount
//! does not return.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::FsClass;

/// A mount as the mount table describes it, with its used and total space when it was asked
/// for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountInfo {
    /// Filesystem type (`zfs`, `ext4`, `nfs4`, `apfs`).
    pub fstype: String,
    pub class: FsClass,
    /// Bytes in use on the filesystem. `None` for network filesystems, which are never asked,
    /// and for those that did not answer within the deadline.
    pub used: Option<u64>,
    /// The filesystem's size, when `used` is known.
    pub total: Option<u64>,
}

/// The mounts at `points`, in the same order: `None` for a point the mount table does not
/// list (or on systems without one). Local filesystems get until `timeout` to report their
/// used space; network ones are not asked.
#[must_use]
pub fn mounts_at(points: &[PathBuf], timeout: Duration) -> Vec<Option<MountInfo>> {
    let table = table();
    let deadline = Instant::now() + timeout;
    let pending: Vec<_> = points
        .iter()
        .map(|p| {
            let (at, fstype) = entry(&table, p)?;
            let class = crate::classify_fs(fstype);
            let rx = match class {
                FsClass::Local => {
                    let at = at.to_path_buf();
                    crate::disks::on_thread(move || space(&at))
                }
                _ => None,
            };
            Some((fstype.clone(), class, rx))
        })
        .collect();
    pending
        .into_iter()
        .map(|p| {
            let (fstype, class, rx) = p?;
            let left = deadline.saturating_duration_since(Instant::now());
            let space = rx
                .and_then(|rx| rx.recv_timeout(left).ok())
                .and_then(Result::ok);
            Some(MountInfo {
                fstype,
                class,
                used: space.map(|s| s.used),
                total: space.map(|s| s.total),
            })
        })
        .collect()
}

/// The last mount at `point`, as given or with its parent's links resolved (the parent is on
/// the filesystem the scan read, so resolving it does not touch the mount).
fn entry<'a>(table: &'a [(PathBuf, String)], point: &Path) -> Option<(&'a Path, &'a String)> {
    let find = |p: &Path| {
        table
            .iter()
            .rev()
            .find(|(at, _)| at == p)
            .map(|(at, t)| (at.as_path(), t))
    };
    find(point).or_else(|| {
        let parent = std::fs::canonicalize(point.parent()?).ok()?;
        find(&parent.join(point.file_name()?))
    })
}

/// Mount points and types, in mount order.
#[cfg(target_os = "linux")]
fn table() -> Vec<(PathBuf, String)> {
    crate::mount_table()
        .map(|t| t.into_iter().map(|e| (e.mount_point, e.fstype)).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn table() -> Vec<(PathBuf, String)> {
    crate::macos::mounted().unwrap_or_default()
}

/// Elsewhere a scan meets no mount it does not enter (Windows volume mount points are
/// reparse points, which are never followed).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn table() -> Vec<(PathBuf, String)> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn space(path: &Path) -> std::io::Result<crate::Space> {
    crate::disks::statvfs_space(path)
}

#[cfg(target_os = "macos")]
fn space(path: &Path) -> std::io::Result<crate::Space> {
    crate::macos::statfs_space(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn space(_path: &Path) -> std::io::Result<crate::Space> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mounts_by_point_or_through_a_resolved_parent() {
        let table = vec![
            (PathBuf::from("/"), "ext4".to_string()),
            (PathBuf::from("/mnt/data"), "ext4".to_string()),
            (PathBuf::from("/mnt/data"), "zfs".to_string()),
        ];
        // A later mount at the same point hides the earlier one.
        let (at, t) = entry(&table, Path::new("/mnt/data")).expect("listed");
        assert_eq!((at, t.as_str()), (Path::new("/mnt/data"), "zfs"));
        assert!(entry(&table, Path::new("/nowhere/at/all")).is_none());
    }

    /// The root filesystem is listed, with its used space; a path that is no mount point is
    /// not.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_root_filesystem_reports_its_use() {
        let got = mounts_at(
            &[PathBuf::from("/"), PathBuf::from("/no/such/mount")],
            Duration::from_secs(5),
        );
        let root = got[0].as_ref().expect("/ is mounted");
        assert_eq!(root.class, FsClass::Local, "{root:?}");
        assert!(root.used.is_some_and(|u| u > 0), "{root:?}");
        assert!(root.total >= root.used, "{root:?}");
        assert_eq!(got[1], None);
    }
}
