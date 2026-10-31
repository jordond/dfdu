//! The disk list on Linux, from the mount table: one entry per filesystem, by device.

#[cfg(target_os = "linux")]
use std::io;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use super::Disk;
#[cfg(target_os = "linux")]
use super::{Space, gather, on_thread};
use crate::{FsClass, MountEntry};

/// The mounted filesystems, sorted by mount point. Each filesystem gets `timeout` to report
/// its size; those that do not answer are listed without one.
///
/// # Errors
///
/// When the mount table cannot be read.
#[cfg(target_os = "linux")]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    let table = crate::mount_table()?;
    let deadline = Instant::now() + timeout;
    let pending = filesystems(&table)
        .into_iter()
        .map(|disk| {
            let mp = disk.mount_point.clone();
            (disk, on_thread(move || statvfs_space(&mp)))
        })
        .collect();
    Ok(gather(pending, deadline))
}

/// The filesystems of the mount table `table`, without their sizes, by device: one entry per
/// filesystem, at the mount of its top directory if there is one, else at its shortest mount
/// point, with its other mount points in `also_at`.
/// Left out: kernel filesystems, mounts below them (`/sys/fs/cgroup`, `/dev/shm`), which a
/// scan never reaches, and mounts hidden by a later mount at the same place.
fn filesystems(table: &[MountEntry]) -> Vec<Disk> {
    use std::collections::BTreeMap;
    let kernel: Vec<&Path> = table
        .iter()
        .filter(|e| crate::classify_fs(&e.fstype) == FsClass::Kernel)
        .map(|e| e.mount_point.as_path())
        .filter(|p| p.parent().is_some())
        .collect();
    let mut by_dev: BTreeMap<u64, Vec<&MountEntry>> = BTreeMap::new();
    for (i, e) in table.iter().enumerate() {
        let hidden = table[i + 1..]
            .iter()
            .any(|later| later.mount_point == e.mount_point);
        let below_kernel = kernel.iter().any(|k| e.mount_point.starts_with(k));
        if !hidden && !below_kernel {
            by_dev.entry(e.st_dev()).or_default().push(e);
        }
    }
    let mut out = vec![];
    for (_, mut mounts) in by_dev {
        mounts.sort_by_key(|e| {
            (
                e.root.as_os_str() != "/",
                e.mount_point.as_os_str().len(),
                e.mount_point.clone(),
            )
        });
        mounts.dedup_by(|a, b| a.mount_point == b.mount_point);
        let first = mounts[0];
        out.push(Disk {
            mount_point: first.mount_point.clone(),
            also_at: mounts[1..].iter().map(|e| e.mount_point.clone()).collect(),
            fstype: first.fstype.clone(),
            source: first.source.clone(),
            class: crate::classify_fs(&first.fstype),
            space: None,
            role: None,
            shares_space_with: vec![],
        });
    }
    out
}

/// The size of the filesystem `path` is on, as `statvfs` reports it.
#[cfg(target_os = "linux")]
pub(crate) fn statvfs_space(path: &Path) -> io::Result<Space> {
    let s = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    let unit = if s.f_frsize > 0 {
        s.f_frsize
    } else {
        s.f_bsize
    };
    Ok(Space {
        total: s.f_blocks.saturating_mul(unit),
        used: s.f_blocks.saturating_sub(s.f_bfree).saturating_mul(unit),
        available: s.f_bavail.saturating_mul(unit),
        purgeable: None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A mount of device `dev` (`major:minor` `8:dev`) showing its directory `root` at `at`.
    fn mount(dev: u32, root: &str, at: &str, fstype: &str) -> MountEntry {
        MountEntry {
            id: 0,
            parent: 0,
            dev: (8, dev),
            root: root.into(),
            mount_point: at.into(),
            fstype: fstype.into(),
            source: format!("/dev/sd{dev}"),
        }
    }

    fn at(disk: &Disk) -> (&Path, Vec<&Path>) {
        let also: Vec<&Path> = disk.also_at.iter().map(PathBuf::as_path).collect();
        (disk.mount_point.as_path(), also)
    }

    /// A filesystem mounted at several places is listed once, at the mount of its top
    /// directory, its subvolumes and bind mounts in `also_at`.
    #[test]
    fn a_filesystem_is_listed_once_at_its_top_directory() {
        let table = [
            mount(1, "/@home", "/home", "btrfs"),
            mount(1, "/", "/mnt/pool", "btrfs"),
            mount(1, "/@", "/", "btrfs"),
            mount(2, "/", "/data", "ext4"),
        ];
        let disks = filesystems(&table);
        let places: Vec<_> = disks.iter().map(at).collect();
        let pool: (&Path, Vec<&Path>) =
            ("/mnt/pool".as_ref(), vec!["/".as_ref(), "/home".as_ref()]);
        assert_eq!(places, [pool, ("/data".as_ref(), vec![])]);
        let pool = &disks[0];
        assert_eq!(
            (pool.fstype.as_str(), pool.source.as_str()),
            ("btrfs", "/dev/sd1")
        );
        assert_eq!(pool.class, FsClass::Local);
        assert!(disks.iter().all(|d| d.space.is_none()));
    }

    /// Without a mount of its top directory, a filesystem is listed at its shortest mount
    /// point.
    #[test]
    fn without_its_top_directory_the_shortest_mount_point_is_listed() {
        let table = [
            mount(1, "/a", "/srv/long/a", "ext4"),
            mount(1, "/b", "/b", "ext4"),
        ];
        let disks = filesystems(&table);
        let places: Vec<_> = disks.iter().map(at).collect();
        let b: (&Path, Vec<&Path>) = ("/b".as_ref(), vec!["/srv/long/a".as_ref()]);
        assert_eq!(places, [b]);
    }

    /// Kernel filesystems and mounts below them are left out, and so is a mount hidden by a
    /// later one at the same place.
    #[test]
    fn kernel_and_hidden_mounts_are_left_out() {
        let table = [
            mount(1, "/", "/", "ext4"),
            mount(2, "/", "/proc", "proc"),
            mount(3, "/", "/sys", "sysfs"),
            mount(4, "/", "/sys/fs/cgroup", "tmpfs"),
            mount(5, "/", "/mnt", "ext4"),
            mount(6, "/", "/mnt", "xfs"),
        ];
        let disks = filesystems(&table);
        let shown: Vec<_> = disks
            .iter()
            .map(|d| (d.mount_point.as_path(), d.fstype.as_str()))
            .collect();
        assert_eq!(
            shown,
            [(Path::new("/"), "ext4"), (Path::new("/mnt"), "xfs")]
        );
    }
}
