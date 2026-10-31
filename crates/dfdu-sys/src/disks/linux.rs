//! The disk list on Linux, from the mount table: one entry per filesystem, by device.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{Disk, Space, gather, on_thread};
use crate::FsClass;

/// The mounted filesystems, sorted by mount point. Each filesystem gets `timeout` to report
/// its size; those that do not answer are listed without one.
///
/// # Errors
///
/// When the mount table cannot be read.
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    use std::collections::BTreeMap;
    let table = crate::mount_table()?;
    // One entry per filesystem, by device: the mount of its top directory if there is one,
    // else the shortest mount point.
    // Left out: kernel filesystems, mounts below them (`/sys/fs/cgroup`, `/dev/shm`), which a
    // scan never reaches, and mounts hidden by a later mount at the same place.
    let kernel: Vec<&Path> = table
        .iter()
        .filter(|e| crate::classify_fs(&e.fstype) == FsClass::Kernel)
        .map(|e| e.mount_point.as_path())
        .filter(|p| p.parent().is_some())
        .collect();
    let mut by_dev: BTreeMap<u64, Vec<&crate::MountEntry>> = BTreeMap::new();
    for (i, e) in table.iter().enumerate() {
        let hidden = table[i + 1..]
            .iter()
            .any(|later| later.mount_point == e.mount_point);
        let below_kernel = kernel.iter().any(|k| e.mount_point.starts_with(k));
        if !hidden && !below_kernel {
            by_dev.entry(e.st_dev()).or_default().push(e);
        }
    }
    let deadline = Instant::now() + timeout;
    let mut pending = vec![];
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
        let mp = first.mount_point.clone();
        let rx = on_thread(move || statvfs_space(&mp));
        let disk = Disk {
            mount_point: first.mount_point.clone(),
            also_at: mounts[1..].iter().map(|e| e.mount_point.clone()).collect(),
            fstype: first.fstype.clone(),
            source: first.source.clone(),
            class: crate::classify_fs(&first.fstype),
            space: None,
            role: None,
            shares_space_with: vec![],
        };
        pending.push((disk, rx));
    }
    Ok(gather(pending, deadline))
}

/// The size of the filesystem `path` is on, as `statvfs` reports it.
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
    })
}
