//! What a directory's data is stored on, for the scanner's thread policy: a spinning disk is
//! read with few threads, since parallel reads there only add seeks.
//!
//! On Linux the answer comes from sysfs: the `queue/rotational` flag of the disk under the
//! mount's block device, following partitions to their disk and device-mapper and md devices
//! to their members. Virtual disks often claim to rotate (virtio, `VMware` and Hyper-V disks set
//! the flag by default, whatever backs them), so a rotating flag on a virtual disk counts as
//! unknown: reading such a disk with few threads would make the common cloud case several
//! times slower, and getting a real spinning disk wrong costs less.

use std::path::Path;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Solid state, or anything else that serves parallel reads well.
    Solid,
    /// A spinning disk.
    Rotational,
    /// A network filesystem.
    Network,
    /// Not known: virtual disks, memory filesystems, systems without the information.
    Unknown,
}

/// The storage behind `path`'s filesystem.
#[cfg(target_os = "linux")]
#[must_use]
pub fn storage(path: &Path) -> Storage {
    let Ok(Some(mount)) = crate::mount_id(path) else {
        return Storage::Unknown;
    };
    let Ok(table) = crate::mount_table() else {
        return Storage::Unknown;
    };
    match table.iter().find(|e| e.id == mount) {
        Some(e) => storage_of_mount(e),
        None => Storage::Unknown,
    }
}

/// The storage behind mount `e`, from the mount table entry and sysfs, without touching the
/// mount itself (a network mount may not answer).
#[cfg(target_os = "linux")]
#[must_use]
pub fn storage_of_mount(e: &crate::MountEntry) -> Storage {
    use rustix::fs::FileType;

    if crate::classify_fs(&e.fstype) == crate::FsClass::Network {
        return Storage::Network;
    }
    // btrfs and others report an anonymous device number; the mount source names the disk.
    let dev = match rustix::fs::stat(e.source.as_str()) {
        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::BlockDevice => {
            (rustix::fs::major(st.st_rdev), rustix::fs::minor(st.st_rdev))
        }
        _ => e.dev,
    };
    storage_in(Path::new("/sys"), dev)
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn storage(_path: &Path) -> Storage {
    Storage::Unknown
}

/// Network filesystems are known from the mount table; nothing else is on these systems.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn storage_of_mount(e: &crate::MountEntry) -> Storage {
    if crate::classify_fs(&e.fstype) == crate::FsClass::Network {
        Storage::Network
    } else {
        Storage::Unknown
    }
}

/// The storage of block device `major:minor`, from the sysfs tree at `sys`.
#[must_use]
pub fn storage_in(sys: &Path, (major, minor): (u32, u32)) -> Storage {
    match std::fs::canonicalize(sys.join(format!("dev/block/{major}:{minor}"))) {
        Ok(dir) => device(&dir, 0),
        Err(_) => Storage::Unknown,
    }
}

fn device(dir: &Path, depth: u32) -> Storage {
    // A partition's queue is its disk's.
    let disk = if dir.join("partition").exists() {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    // Device mapper, md, bcache: the members decide. Any rotating member makes the whole
    // rotate; all solid makes it solid.
    if depth < 8
        && let Ok(rd) = std::fs::read_dir(disk.join("slaves"))
    {
        let members: Vec<Storage> = rd
            .flatten()
            .filter_map(|e| std::fs::canonicalize(e.path()).ok())
            .map(|p| device(&p, depth + 1))
            .collect();
        if !members.is_empty() {
            return if members.contains(&Storage::Rotational) {
                Storage::Rotational
            } else if members.iter().all(|&m| m == Storage::Solid) {
                Storage::Solid
            } else {
                Storage::Unknown
            };
        }
    }
    let flag = std::fs::read_to_string(disk.join("queue/rotational")).unwrap_or_default();
    match flag.trim() {
        "0" => Storage::Solid,
        "1" if !is_virtual(disk) => Storage::Rotational,
        _ => Storage::Unknown,
    }
}

/// Disks of a hypervisor, a loop or memory device, or a cloud provider.
fn is_virtual(disk: &Path) -> bool {
    let path = disk.to_string_lossy().to_ascii_lowercase();
    if ["/virtio", "/xen", "/vmbus", "/devices/virtual/"]
        .iter()
        .any(|s| path.contains(s))
    {
        return true;
    }
    let id = ["device/vendor", "device/model"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(disk.join(f)).ok())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "qemu", "vmware", "vbox", "virtual", "msft", "google", "amazon", "xen",
    ]
    .iter()
    .any(|s| id.contains(s))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// A disk under `devices/<bus>` with a partition, linked from `dev/block`.
    fn disk(sys: &Path, bus: &str, name: &str, major: u32, rotational: &str, vendor: &str) {
        let d = sys.join("devices").join(bus).join("block").join(name);
        fs::create_dir_all(d.join("queue")).unwrap();
        fs::create_dir_all(d.join("device")).unwrap();
        fs::write(d.join("queue/rotational"), format!("{rotational}\n")).unwrap();
        fs::write(d.join("device/vendor"), vendor).unwrap();
        let p = d.join(format!("{name}1"));
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("partition"), "1").unwrap();
        fs::create_dir_all(sys.join("dev/block")).unwrap();
        symlink(&d, sys.join(format!("dev/block/{major}:0"))).unwrap();
        symlink(&p, sys.join(format!("dev/block/{major}:1"))).unwrap();
    }

    #[test]
    fn classifies_disks_partitions_and_virtual_disks() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path();
        disk(sys, "pci0/ata1/host0", "sda", 8, "1", "ATA     ");
        disk(sys, "pci0/nvme", "nvme0n1", 259, "0", "");
        disk(sys, "pci0/virtio1", "vda", 254, "1", "0x1af4");
        disk(sys, "pci0/host2/target2", "sdb", 65, "1", "VMware  ");
        assert_eq!(storage_in(sys, (8, 0)), Storage::Rotational);
        assert_eq!(storage_in(sys, (8, 1)), Storage::Rotational, "partition");
        assert_eq!(storage_in(sys, (259, 1)), Storage::Solid);
        assert_eq!(storage_in(sys, (254, 0)), Storage::Unknown, "virtio");
        assert_eq!(storage_in(sys, (65, 1)), Storage::Unknown, "VMware SCSI");
        assert_eq!(storage_in(sys, (1, 1)), Storage::Unknown, "no such device");

        // A device-mapper volume over a rotating partition and a solid disk.
        let dm = sys.join("devices/virtual/block/dm-0");
        fs::create_dir_all(dm.join("slaves")).unwrap();
        fs::create_dir_all(dm.join("queue")).unwrap();
        fs::write(dm.join("queue/rotational"), "0").unwrap();
        symlink(
            sys.join("devices/pci0/nvme/block/nvme0n1/nvme0n11"),
            dm.join("slaves/nvme0n1p1"),
        )
        .unwrap();
        symlink(&dm, sys.join("dev/block/253:0")).unwrap();
        assert_eq!(storage_in(sys, (253, 0)), Storage::Solid);
        symlink(
            sys.join("devices/pci0/ata1/host0/block/sda/sda1"),
            dm.join("slaves/sda1"),
        )
        .unwrap();
        assert_eq!(storage_in(sys, (253, 0)), Storage::Rotational);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_temp_dir_has_some_answer() {
        // Whatever this machine is; the call must not fail or hang.
        let _ = storage(&std::env::temp_dir());
    }
}
