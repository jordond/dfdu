//! Mounted filesystems with their sizes, for the disk list (`dut disks`, the browser's picker).
//! Like `df`: kernel filesystems and those without blocks are left out, and a filesystem
//! mounted at several places (btrfs subvolumes, bind mounts) is listed once.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::FsClass;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    pub mount_point: PathBuf,
    /// Other places the same filesystem is mounted.
    pub also_at: Vec<PathBuf>,
    /// Filesystem type (`ext4`, `btrfs`, `nfs4`, `apfs`, `NTFS`).
    pub fstype: String,
    /// What is mounted (`/dev/sda1`, `server:/export`), when known.
    pub source: String,
    pub class: FsClass,
    /// `None` when the filesystem did not answer in time (a hung network mount).
    pub space: Option<Space>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub used: u64,
    /// Free space available to this user (less than total minus used where blocks are
    /// reserved for root).
    pub available: u64,
}

/// Runs `f` on its own thread; its result arrives on the returned channel. The caller waits
/// with a timeout, and a call that does not return (a hung network mount does not let go) is
/// left running.
fn on_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<mpsc::Receiver<T>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("dut-disk-space".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    Some(rx)
}

/// The mounted filesystems, sorted by mount point. Each filesystem gets `timeout` to report
/// its size; those that do not answer are listed without one.
#[cfg(target_os = "linux")]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    use std::collections::BTreeMap;
    let table = crate::mount_table()?;
    // One entry per filesystem, by device: the mount of its top directory if there is one,
    // else the shortest mount point.
    // Left out: kernel filesystems, mounts below them (`/sys/fs/cgroup`, `/dev/shm`), which a
    // scan never reaches, and mounts hidden by a later mount at the same place.
    let kernel: Vec<&std::path::Path> = table
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
        };
        pending.push((disk, rx));
    }
    let mut out = vec![];
    for (mut disk, rx) in pending {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.map(|rx| rx.recv_timeout(left)) {
            Some(Ok(Ok(space))) => disk.space = Some(space),
            // Gone, or not ours to look at: leave it out.
            Some(Ok(Err(_))) => continue,
            // Did not answer.
            _ => {}
        }
        if disk.space.is_some_and(|s| s.total == 0) {
            continue;
        }
        out.push(disk);
    }
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    Ok(out)
}

#[cfg(target_os = "linux")]
fn statvfs_space(path: &std::path::Path) -> io::Result<Space> {
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

/// The mounted filesystems, sorted by mount point, from `sysinfo`. The whole listing gets
/// `timeout`.
#[cfg(not(target_os = "linux"))]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    let deadline = Instant::now() + timeout;
    let rx = on_thread(|| {
        sysinfo::Disks::new_with_refreshed_list()
            .list()
            .iter()
            .map(|d| {
                let fstype = d.file_system().to_string_lossy().into_owned();
                let (total, available) = (d.total_space(), d.available_space());
                Disk {
                    mount_point: d.mount_point().to_path_buf(),
                    also_at: vec![],
                    class: crate::classify_fs(&fstype),
                    fstype,
                    source: d.name().to_string_lossy().into_owned(),
                    space: Some(Space {
                        total,
                        used: total.saturating_sub(available),
                        available,
                    }),
                }
            })
            .collect::<Vec<_>>()
    })
    .ok_or_else(|| io::Error::other("could not start a thread"))?;
    let mut out = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the disk list did not answer"))?;
    out.retain(|d| d.class != FsClass::Kernel && d.space.is_some_and(|s| s.total > 0));
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_root_filesystem_once() {
        let disks = disks(Duration::from_secs(5)).expect("disks");
        let root = std::path::Path::new(if cfg!(windows) { "C:\\" } else { "/" });
        let n = disks
            .iter()
            .filter(|d| d.mount_point == root || d.also_at.iter().any(|p| p == root))
            .count();
        assert_eq!(n, 1, "{disks:#?}");
        assert!(disks.iter().all(|d| d.class != FsClass::Kernel));
        for d in disks.iter().filter_map(|d| d.space) {
            assert!(d.used <= d.total && d.available <= d.total, "{d:?}");
        }
    }
}
