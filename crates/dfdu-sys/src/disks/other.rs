//! The disk list on the other systems, Windows among them, from `sysinfo`.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::time::Duration;
use std::time::Instant;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use super::on_thread;
use super::{Disk, Space};
use crate::FsClass;

/// The mounted filesystems, sorted by mount point, from `sysinfo`. The whole listing gets
/// `timeout`.
///
/// # Errors
///
/// When no thread can be started for the listing, when `sysinfo` cannot list the disks, or
/// `TimedOut` when the listing takes longer than `timeout`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    let deadline = Instant::now() + timeout;
    let rx = on_thread(|| {
        let disks = sysinfo::Disks::new_with_refreshed_list().map_err(|e| match e {
            sysinfo::Error::Io(e) => e,
            e => io::Error::other(e),
        })?;
        let list = disks
            .iter()
            .map(|d| {
                disk(
                    d.mount_point().to_path_buf(),
                    d.file_system().to_string_lossy().into_owned(),
                    d.name().to_string_lossy().into_owned(),
                    d.total_space(),
                    d.available_space(),
                )
            })
            .collect::<Vec<_>>();
        Ok::<_, io::Error>(list)
    })
    .ok_or_else(|| io::Error::other("could not start a thread"))?;
    Ok(listing(answer(&rx, deadline)?))
}

/// A disk as `sysinfo` describes it: mounted at `mount_point` from `source`, of `total` bytes
/// with `available` of them free, the rest counted as used. On Windows `sysinfo`'s source is
/// the volume label, which is the disk's label too when there is one; elsewhere it is the
/// device.
fn disk(mount_point: PathBuf, fstype: String, source: String, total: u64, available: u64) -> Disk {
    Disk {
        mount_point,
        also_at: vec![],
        class: crate::classify_fs(&fstype),
        fstype,
        label: label(&source, cfg!(windows)),
        source,
        space: Some(Space {
            total,
            used: total.saturating_sub(available),
            available,
            purgeable: None,
        }),
        role: None,
        shares_space_with: vec![],
    }
}

/// The label of a disk whose `sysinfo` source is `source`: the source itself on Windows
/// (`windows`), where it is the volume label, unless the volume has none.
fn label(source: &str, windows: bool) -> Option<String> {
    (windows && !source.is_empty()).then(|| source.to_owned())
}

/// The listing that arrives on `rx` by `deadline`.
///
/// # Errors
///
/// The listing's own error, or `TimedOut` when it did not arrive in time (or its thread ended
/// without one).
fn answer(rx: &mpsc::Receiver<io::Result<Vec<Disk>>>, deadline: Instant) -> io::Result<Vec<Disk>> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the disk list did not answer"))?
}

/// The disks the list shows, sorted by mount point: kernel filesystems and filesystems
/// without blocks are left out.
fn listing(mut disks: Vec<Disk>) -> Vec<Disk> {
    disks.retain(|d| d.class != FsClass::Kernel && d.space.is_some_and(|s| s.total > 0));
    disks.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    disks
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(mount_point: &str, fstype: &str, total: u64) -> Disk {
        disk(mount_point.into(), fstype.into(), "src".into(), total, 0)
    }

    /// What is not free counts as used, and the filesystem type gives the class.
    #[test]
    fn a_disk_counts_what_is_not_free_as_used() {
        let d = disk("C:\\".into(), "NTFS".into(), "Windows".into(), 1000, 300);
        let space = Space {
            total: 1000,
            used: 700,
            available: 300,
            purgeable: None,
        };
        assert_eq!(d.space, Some(space));
        assert_eq!((d.class, d.fstype.as_str()), (FsClass::Local, "NTFS"));
        assert_eq!(d.source, "Windows");
        // A filesystem reporting more free space than its size has none used.
        let odd = disk("D:\\".into(), "NTFS".into(), String::new(), 100, 200);
        assert_eq!(odd.space.map(|s| s.used), Some(0));
    }

    /// On Windows the source is the volume label; elsewhere it is a device, no label.
    #[test]
    fn only_windows_sources_are_labels() {
        assert_eq!(label("Windows", true).as_deref(), Some("Windows"));
        assert_eq!(label("", true), None);
        assert_eq!(label("/dev/ada0p2", false), None);
    }

    /// Kernel filesystems and those without blocks are left out, the rest sorted by mount
    /// point.
    #[test]
    fn the_list_leaves_out_kernel_and_empty_filesystems() {
        let disks = vec![
            at("/z", "ext4", 10),
            at("/proc", "proc", 10),
            at("/empty", "ext4", 0),
            at("/a", "ext4", 10),
        ];
        let shown: Vec<_> = listing(disks).into_iter().map(|d| d.mount_point).collect();
        assert_eq!(shown, [PathBuf::from("/a"), PathBuf::from("/z")]);
    }

    /// A listing that does not arrive by the deadline, or whose thread ends without one, times
    /// out; one that does arrives with its own error, if any.
    #[test]
    fn a_listing_that_does_not_arrive_in_time_times_out() {
        let (tx, rx) = mpsc::channel();
        let soon = Instant::now() + Duration::from_millis(10);
        let late = answer(&rx, soon).expect_err("nothing sent");
        assert_eq!(late.kind(), io::ErrorKind::TimedOut);
        tx.send(Err(io::Error::from(io::ErrorKind::PermissionDenied)))
            .unwrap();
        let failed = answer(&rx, Instant::now()).expect_err("sent an error");
        assert_eq!(failed.kind(), io::ErrorKind::PermissionDenied);
        tx.send(Ok(vec![at("/a", "ext4", 1)])).unwrap();
        assert_eq!(answer(&rx, Instant::now()).unwrap().len(), 1);
        drop(tx);
        let gone = answer(&rx, Instant::now() + Duration::from_secs(5)).expect_err("gone");
        assert_eq!(gone.kind(), io::ErrorKind::TimedOut);
    }
}
