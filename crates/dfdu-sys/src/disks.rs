//! Mounted filesystems with their sizes, for the disk list (`dfdu disks`, the browser's picker).
//! Like `df`: kernel filesystems and those without blocks are left out, and a filesystem
//! mounted at several places (btrfs subvolumes, bind mounts) is listed once. Each system's
//! listing is in a module of its own: `linux`, `macos` and, from `sysinfo`, `other`.

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
mod other;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Instant;

#[cfg(target_os = "linux")]
pub use linux::disks;
#[cfg(target_os = "linux")]
pub(crate) use linux::statvfs_space;
#[cfg(target_os = "macos")]
pub(crate) use macos::Volume;
#[cfg(target_os = "macos")]
pub use macos::disks;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use other::disks;

use crate::FsClass;

/// How long each filesystem gets to report its size in the disk list: the `timeout` of
/// [`disks`] for `dfdu disks` and the browser's picker.
pub const DISK_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// What the volume holds, where the system says (macOS).
    pub role: Option<Role>,
    /// Other disks listed that draw on the same space (APFS volumes of one container): the
    /// size, use and free space shown are the container's, for each of them.
    pub shares_space_with: Vec<PathBuf>,
}

/// What a volume holds. macOS starts from a sealed, read-only system volume and keeps
/// everything that changes on a data volume of the same APFS container, which the system's
/// tree reaches through firmlinks (`/Users` is `/System/Volumes/Data/Users`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    Data,
}

impl Role {
    /// How the disk list names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Role::System => "System (read-only)",
            Role::Data => "Data",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub used: u64,
    /// Free space available to this user (less than total minus used where blocks are
    /// reserved for root): `f_bavail`, as `df` shows it.
    pub available: u64,
    /// macOS: what the system can free on top of `available` when it needs the room (caches,
    /// local snapshots, iCloud files it can evict), which the Finder counts as available.
    /// `None` on other systems, or when the system did not say.
    pub purgeable: Option<u64>,
}

/// Runs `f` on its own thread; its result arrives on the returned channel. The caller waits
/// with a timeout, and a call that does not return (a hung network mount does not let go) is
/// left running.
pub(crate) fn on_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<mpsc::Receiver<T>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("dfdu-disk-space".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    Some(rx)
}

/// A disk whose size is being asked for on its own thread ([`on_thread`]).
#[cfg(any(target_os = "linux", target_os = "macos"))]
type Pending = (Disk, Option<mpsc::Receiver<io::Result<Space>>>);

/// The disks of `pending` with the sizes that arrived by `deadline`, sorted by mount point.
/// A disk that did not answer is listed without a size; one that failed or has no blocks is
/// left out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn gather(pending: Vec<Pending>, deadline: Instant) -> Vec<Disk> {
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
    out
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
