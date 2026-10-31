//! The disk list on the other systems, Windows among them, from `sysinfo`.

use std::io;
use std::time::{Duration, Instant};

use super::{Disk, Space, on_thread};
use crate::FsClass;

/// The mounted filesystems, sorted by mount point, from `sysinfo`. The whole listing gets
/// `timeout`.
///
/// # Errors
///
/// When no thread can be started for the listing, when `sysinfo` cannot list the disks, or
/// `TimedOut` when the listing takes longer than `timeout`.
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
                    role: None,
                    shares_space_with: vec![],
                }
            })
            .collect::<Vec<_>>();
        Ok::<_, io::Error>(list)
    })
    .ok_or_else(|| io::Error::other("could not start a thread"))?;
    let mut out = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the disk list did not answer"))??;
    out.retain(|d| d.class != FsClass::Kernel && d.space.is_some_and(|s| s.total > 0));
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    Ok(out)
}
