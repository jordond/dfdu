//! The disk list on macOS, from the volumes the kernel's mount table describes: those the
//! Finder shows, with their roles and the APFS containers whose space they share. The choice of
//! volumes is tested on every system.

#[cfg(target_os = "macos")]
use std::io;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use super::{Disk, Role, Space};
#[cfg(target_os = "macos")]
use super::{gather, on_thread};
use crate::FsClass;

/// The mounted volumes the Finder shows, sorted by mount point, from the kernel's mount table
/// (`getfsstat`). Each gets `timeout` to report its size; those that do not answer are listed
/// without one.
///
/// # Errors
///
/// When the mount table cannot be read.
#[cfg(target_os = "macos")]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    let volumes = crate::macos::volumes()?;
    let deadline = Instant::now() + timeout;
    let pending = listed(&volumes)
        .map(|v| {
            let at = v.mount_point.clone();
            let rx = on_thread(move || {
                let space = crate::macos::statfs_space(&at)?;
                let important = crate::macos::important_available(&at);
                Ok(with_purgeable(container_space(space), important))
            });
            let disk = Disk {
                mount_point: v.mount_point.clone(),
                also_at: vec![],
                fstype: v.fstype.clone(),
                source: v.from.clone(),
                label: None,
                class: crate::classify_fs(&v.fstype),
                space: None,
                role: None,
                shares_space_with: vec![],
            };
            (disk, rx)
        })
        .collect();
    // The names are asked for on threads of their own, like the sizes, by the same deadline.
    let mut names: Vec<_> = listed(&volumes)
        .map(|v| {
            let at = v.mount_point.clone();
            (
                v.mount_point.clone(),
                on_thread(move || crate::macos::volume_name(&at)),
            )
        })
        .collect();
    let mut out = gather(pending, deadline);
    describe_volumes(&mut out, &volumes);
    for d in &mut out {
        let Some(i) = names.iter().position(|(at, _)| *at == d.mount_point) else {
            continue;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        d.label = names
            .swap_remove(i)
            .1
            .and_then(|rx| rx.recv_timeout(left).ok())
            .flatten();
    }
    Ok(out)
}

/// The space of a volume as the disk list shows it: what `statfs` reports, with the used part
/// being all that is not available. For an APFS volume that is the whole container's use, the
/// same on each of its volumes ([`Disk::shares_space_with`]); `statfs` counts only the
/// volume's own blocks as used.
///
/// The free space is `f_bavail`, as `df` shows it: what can be written now. The Finder shows
/// more, counting purgeable data as available ([`with_purgeable`]).
#[cfg(target_os = "macos")]
fn container_space(s: Space) -> Space {
    Space {
        used: s.total.saturating_sub(s.available),
        ..s
    }
}

/// `s` with its purgeable space: what the system says is available for important use (the
/// Finder's free space, `important`) beyond `f_bavail`. The system frees it (caches, local
/// snapshots, iCloud files it can evict) only when it needs the room.
#[cfg(any(target_os = "macos", test))]
fn with_purgeable(s: Space, important: Option<u64>) -> Space {
    Space {
        purgeable: important.map(|i| i.saturating_sub(s.available)),
        ..s
    }
}

/// The volumes the disk list shows: those the Finder lists (local, not mounted `nobrowse`
/// except the system's data volume, which the Finder reaches through the system volume) and
/// not kernel filesystems, each mount point once (the last mount there hides the others).
fn listed(volumes: &[Volume]) -> impl Iterator<Item = &Volume> {
    volumes.iter().enumerate().filter_map(|(i, v)| {
        let hidden = volumes[i + 1..]
            .iter()
            .any(|later| later.mount_point == v.mount_point);
        let shown = v.local && (v.browsable || v.role == Some(Role::Data));
        (shown && !hidden && crate::classify_fs(&v.fstype) != FsClass::Kernel).then_some(v)
    })
}

/// A mounted volume as it describes itself: where it is mounted, what from, and its role.
pub(crate) struct Volume {
    pub mount_point: PathBuf,
    /// The device (`/dev/disk3s5`; `/dev/disk3s1s1` for a snapshot of `disk3s1`).
    pub from: String,
    /// Filesystem type (`apfs`, `hfs`, `smbfs`).
    pub fstype: String,
    pub role: Option<Role>,
    /// Not mounted `nobrowse`: the Finder shows it.
    pub browsable: bool,
    /// On a device of this machine, not over the network.
    pub local: bool,
}

/// The APFS container a volume's device is part of: `disk3` for `/dev/disk3s5`.
fn container(dev: &str) -> Option<&str> {
    let name = dev.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("disk")?;
    let n = digits.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0 && digits.as_bytes().get(n) == Some(&b's')).then(|| &name[..4 + n])
}

/// Sets each disk's role, and the other disks on the same APFS container, from `volumes`.
fn describe_volumes(disks: &mut [Disk], volumes: &[Volume]) {
    let of = |d: &Disk| {
        volumes
            .iter()
            .rev()
            .find(|v| v.mount_point == d.mount_point)
    };
    let containers: Vec<Option<&str>> = disks
        .iter()
        .map(|d| {
            of(d)
                .filter(|_| d.fstype == "apfs")
                .and_then(|v| container(&v.from))
        })
        .collect();
    for i in 0..disks.len() {
        disks[i].role = of(&disks[i]).and_then(|v| v.role);
        let Some(c) = containers[i] else { continue };
        let others: Vec<PathBuf> = (0..disks.len())
            .filter(|&j| j != i && containers[j] == Some(c))
            .map(|j| disks[j].mount_point.clone())
            .collect();
        disks[i].shares_space_with = others;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(at: &str, fstype: &str) -> Disk {
        Disk {
            mount_point: at.into(),
            also_at: vec![],
            fstype: fstype.into(),
            source: String::new(),
            label: None,
            class: FsClass::Local,
            space: None,
            role: None,
            shares_space_with: vec![],
        }
    }

    fn volume(at: &str, from: &str, role: Option<Role>) -> Volume {
        Volume {
            mount_point: at.into(),
            from: from.into(),
            fstype: "apfs".into(),
            role,
            browsable: true,
            local: true,
        }
    }

    /// The Finder's volumes are listed: the hidden system volumes are not, except the data
    /// volume; nor network or kernel filesystems, nor a mount hidden by a later one.
    #[test]
    fn lists_the_volumes_the_finder_shows() {
        let hidden = |v: Volume| Volume {
            browsable: false,
            ..v
        };
        let volumes = [
            volume("/", "/dev/disk3s1s1", Some(Role::System)),
            Volume {
                fstype: "devfs".into(),
                ..hidden(volume("/dev", "devfs", None))
            },
            hidden(volume("/System/Volumes/VM", "/dev/disk3s6", None)),
            hidden(volume(
                "/System/Volumes/Data",
                "/dev/disk3s5",
                Some(Role::Data),
            )),
            Volume {
                fstype: "autofs".into(),
                ..hidden(volume("/System/Volumes/Data/home", "map auto_home", None))
            },
            Volume {
                fstype: "smbfs".into(),
                local: false,
                ..volume("/Volumes/share", "//me@nas/share", None)
            },
            volume("/Volumes/USB", "/dev/disk6s1", None),
            Volume {
                fstype: "msdos".into(),
                ..volume("/Volumes/USB", "/dev/disk7s1", None)
            },
        ];
        let got: Vec<(&str, &str)> = listed(&volumes)
            .map(|v| (v.mount_point.to_str().unwrap(), v.from.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("/", "/dev/disk3s1s1"),
                ("/System/Volumes/Data", "/dev/disk3s5"),
                ("/Volumes/USB", "/dev/disk7s1"),
            ]
        );
    }

    /// Purgeable is what the Finder counts as available beyond `f_bavail`, never below zero.
    #[test]
    fn purgeable_is_the_finders_free_space_beyond_f_bavail() {
        let s = Space {
            total: 1000,
            used: 700,
            available: 300,
            purgeable: None,
        };
        assert_eq!(with_purgeable(s, Some(450)).purgeable, Some(150));
        assert_eq!(with_purgeable(s, Some(200)).purgeable, Some(0));
        assert_eq!(with_purgeable(s, None).purgeable, None);
        assert_eq!(with_purgeable(s, Some(450)).available, 300);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_root_volume_reports_its_important_capacity() {
        let important = crate::macos::important_available(std::path::Path::new("/"));
        assert!(important.is_some_and(|i| i > 0), "{important:?}");
    }

    #[test]
    fn containers_come_from_the_device_name() {
        assert_eq!(container("/dev/disk3s5"), Some("disk3"));
        assert_eq!(container("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(container("/dev/disk12s2"), Some("disk12"));
        assert_eq!(container("/dev/disk4"), None);
        assert_eq!(container("map auto_home"), None);
        assert_eq!(container("/dev/sda1"), None);
    }

    #[test]
    fn apfs_volumes_of_one_container_share_space_and_keep_their_roles() {
        let mut disks = vec![
            disk("/", "apfs"),
            disk("/System/Volumes/Data", "apfs"),
            disk("/Volumes/Backup", "apfs"),
            disk("/Volumes/USB", "msdos"),
        ];
        let volumes = [
            volume("/", "/dev/disk3s1s1", Some(Role::System)),
            volume("/System/Volumes/Data", "/dev/disk3s5", Some(Role::Data)),
            volume("/System/Volumes/VM", "/dev/disk3s6", None),
            volume("/Volumes/Backup", "/dev/disk5s1", None),
            volume("/Volumes/USB", "/dev/disk6s1", None),
        ];
        describe_volumes(&mut disks, &volumes);
        let roles: Vec<Option<Role>> = disks.iter().map(|d| d.role).collect();
        assert_eq!(roles, [Some(Role::System), Some(Role::Data), None, None]);
        assert_eq!(
            disks[0].shares_space_with,
            [PathBuf::from("/System/Volumes/Data")]
        );
        assert_eq!(disks[1].shares_space_with, [PathBuf::from("/")]);
        let none: [PathBuf; 0] = [];
        assert_eq!(disks[2].shares_space_with, none);
        assert_eq!(disks[3].shares_space_with, none);
    }

    /// The system volume and its data volume are listed with their roles and the space of
    /// their container, which they share.
    #[cfg(target_os = "macos")]
    #[test]
    fn lists_the_system_and_data_volumes_with_their_container() {
        let disks = disks(Duration::from_secs(5)).expect("disks");
        let find = |at: &str| {
            disks
                .iter()
                .find(|d| d.mount_point == std::path::Path::new(at))
                .unwrap_or_else(|| panic!("{at} not listed: {disks:#?}"))
        };
        let (system, data) = (find("/"), find("/System/Volumes/Data"));
        assert_eq!(
            (system.role, data.role),
            (Some(Role::System), Some(Role::Data))
        );
        assert_eq!(system.fstype, "apfs");
        assert!(system.source.starts_with("/dev/disk"), "{system:?}");
        assert!(
            system.shares_space_with.contains(&data.mount_point),
            "{system:?}"
        );
        assert!(
            data.shares_space_with.contains(&system.mount_point),
            "{data:?}"
        );
        for s in [system.space, data.space] {
            let s = s.expect("answered");
            assert_eq!(s.used + s.available, s.total, "{s:?}");
        }
        assert_eq!(system.space.map(|s| s.total), data.space.map(|s| s.total));
        // The Finder names both after the system (`Macintosh HD`).
        assert!(system.label.is_some(), "{system:?}");
        assert_eq!(system.label, data.label);
    }
}
