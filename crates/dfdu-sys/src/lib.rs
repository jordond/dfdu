//! OS calls that no safe crate provides, behind safe wrappers: the btrfs ioctls and FIEMAP on
//! Linux, `getattrlistbulk` on macOS, and the directory enumeration and file information calls
//! of the Windows backend. Syscalls with safe rustix APIs are called directly. Each ioctl or
//! `getattrlistbulk` argument struct is checked against the kernel's size or `libc`'s
//! definitions.
//!
//! On other systems the functions report "not available": `Ok(None)` for lookups (`mount_id`,
//! `subvol_info`), `Ok(false)` for yes/no questions (`is_btrfs`, `shares_extents`), and
//! `Unsupported` where no answer can be given (`mount_table`).

mod disks;
mod mounted;
mod profile;
mod storage;
pub use disks::{DISK_TIMEOUT, Disk, Role, Space, disks};
pub use mounted::{MountInfo, mounts_at};
pub use profile::Profile;
pub use storage::{Storage, storage, storage_in, storage_of_mount};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::*;

#[cfg(any(target_os = "macos", test))]
mod bulk;
#[cfg(target_os = "macos")]
pub use bulk::{BulkEntry, SF_FIRMLINK, VDIR, VLNK, VREG, parse_bulk_record};
#[cfg(target_os = "macos")]
pub mod fsevents;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    CloneAt, CloneInfo, clone_info, clone_info_at, performance_cores, read_bulk, shares_extents,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
#[cfg(windows)]
mod usn;
#[cfg(windows)]
pub use usn::{UsnChanges, usn_changes_since, usn_position};

/// Whether this process runs as root, so it may read every directory, delete read-only btrfs
/// snapshots and read extent trees.
#[cfg(unix)]
#[must_use]
pub fn is_privileged() -> bool {
    rustix::process::geteuid().is_root()
}

#[cfg(not(any(unix, windows)))]
pub fn is_privileged() -> bool {
    false
}

/// A btrfs subvolume root, as `BTRFS_IOC_GET_SUBVOL_INFO` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubvolInfo {
    pub id: u64,
    /// Id of the subvolume this one's root directory is in (5 for the top level).
    pub parent_id: u64,
    pub name: Vec<u8>,
    pub uuid: [u8; 16],
    /// Set for snapshots: the uuid of the subvolume the snapshot was taken of.
    pub parent_uuid: Option<[u8; 16]>,
    pub read_only: bool,
    /// Creation time, seconds since the Unix epoch.
    pub otime: i64,
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountEntry {
    pub id: u64,
    pub parent: u64,
    /// Device of the filesystem (`major:minor`); the same for every mount of it.
    pub dev: (u32, u32),
    /// Directory of the filesystem shown at the mount point (`/sub` for a subvolume or bind
    /// mount of `sub`).
    pub root: std::path::PathBuf,
    pub mount_point: std::path::PathBuf,
    /// Filesystem type (`btrfs`, `nfs4`, `fuse.sshfs`, `proc`).
    pub fstype: String,
    /// What is mounted (`/dev/sda1`, `server:/export`).
    pub source: String,
}

impl MountEntry {
    /// [`MountEntry::dev`] as `st_dev` reports it (glibc's `makedev` encoding).
    #[must_use]
    pub fn st_dev(&self) -> u64 {
        let (ma, mi) = (u64::from(self.dev.0), u64::from(self.dev.1));
        ((ma & 0xffff_f000) << 32) | ((ma & 0xfff) << 8) | ((mi & 0xffff_ff00) << 12) | (mi & 0xff)
    }
}

/// What kind of filesystem a mount is, for deciding whether to scan it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FsClass {
    /// Disks, memory filesystems (tmpfs), overlays.
    Local,
    /// Remote filesystems: slow, can hang, and their data is not on this machine. Scanned only
    /// when asked for.
    Network,
    /// Kernel interfaces (`/proc`, `/sys`, cgroups): no data, sizes that mean nothing. Never
    /// scanned below the scan root.
    Kernel,
}

/// Classifies a filesystem type as `/proc/self/mountinfo` names it.
#[must_use]
pub fn classify_fs(fstype: &str) -> FsClass {
    const KERNEL: &[&str] = &[
        "proc",
        "sysfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "debugfs",
        "tracefs",
        "securityfs",
        "pstore",
        "bpf",
        "configfs",
        "fusectl",
        "mqueue",
        "hugetlbfs",
        "binfmt_misc",
        "efivarfs",
        "autofs",
        "rpc_pipefs",
        "nsfs",
        "selinuxfs",
        "nfsd",
    ];
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "ncpfs",
        "afs",
        "ceph",
        "glusterfs",
        "9p",
        "lustre",
        "gpfs",
        "afpfs",
        "webdav",
        "davfs",
        "coda",
        "orangefs",
        "beegfs",
    ];
    // FUSE filesystems that reach over the network.
    const NETWORK_FUSE: &[&str] = &[
        "sshfs",
        "rclone",
        "s3fs",
        "gcsfuse",
        "goofys",
        "juicefs",
        "davfs",
        "glusterfs",
        "ceph-fuse",
        "gvfsd-fuse",
        "curlftpfs",
        "smbnetfs",
        "onedriver",
    ];
    let sub = fstype
        .strip_prefix("fuse.")
        .or_else(|| fstype.strip_prefix("fuseblk."));
    if KERNEL.contains(&fstype) {
        FsClass::Kernel
    } else if NETWORK.contains(&fstype) || sub.is_some_and(|s| NETWORK_FUSE.contains(&s)) {
        FsClass::Network
    } else {
        FsClass::Local
    }
}

/// Parses `/proc/self/mountinfo` text. Malformed lines are skipped.
#[must_use]
pub fn parse_mountinfo(text: &[u8]) -> Vec<MountEntry> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let mut f = line.split(|&b| b == b' ');
            let id = std::str::from_utf8(f.next()?).ok()?.parse().ok()?;
            let parent = std::str::from_utf8(f.next()?).ok()?.parse().ok()?;
            let (maj, min) = std::str::from_utf8(f.next()?).ok()?.split_once(':')?;
            let dev = (maj.parse().ok()?, min.parse().ok()?);
            let root = unescape(f.next()?)?;
            let mount_point = unescape(f.next()?)?;
            // Optional fields end at a lone "-"; the type and source follow.
            let mut f = f.skip_while(|x| *x != b"-").skip(1);
            let fstype = String::from_utf8_lossy(f.next()?).into_owned();
            let source = String::from_utf8_lossy(
                &unescape(f.next()?)?.into_os_string().into_encoded_bytes(),
            )
            .into_owned();
            Some(MountEntry {
                id,
                parent,
                dev,
                root,
                mount_point,
                fstype,
                source,
            })
        })
        .collect()
}

/// Octal escapes (`\040` for a space) as the kernel writes them.
fn unescape(field: &[u8]) -> Option<std::path::PathBuf> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 3 < field.len() {
            let oct = std::str::from_utf8(&field[i + 1..i + 4]).ok()?;
            out.push(u8::from_str_radix(oct, 8).ok()?);
            i += 4;
        } else {
            out.push(field[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(std::ffi::OsString::from_vec(out).into())
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(out).ok().map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn privileged_matches_effective_uid() {
        let out = std::process::Command::new("id").arg("-u").output().unwrap();
        let root = String::from_utf8_lossy(&out.stdout).trim() == "0";
        assert_eq!(is_privileged(), root);
    }

    #[test]
    fn mountinfo_lines() {
        let text = b"50 28 0:41 / /srv/b\\040t rw,relatime shared:1 - btrfs /dev/loop0 rw\n\
                     51 50 0:41 /sub /srv/b\\040t/mnt rw - btrfs /dev/loop0 rw\nbroken\n";
        let m = parse_mountinfo(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].mount_point, std::path::Path::new("/srv/b t"));
        assert_eq!((m[1].id, m[1].parent, m[1].dev), (51, 50, (0, 41)));
        assert_eq!(m[1].root, std::path::Path::new("/sub"));
        assert_eq!(m[1].mount_point, std::path::Path::new("/srv/b t/mnt"));
        assert_eq!(
            (m[0].fstype.as_str(), m[0].source.as_str()),
            ("btrfs", "/dev/loop0")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_devices_match_stat() {
        use std::os::unix::fs::MetadataExt;
        let table = mount_table().expect("mountinfo");
        for mp in ["/", "/proc"] {
            let Some(e) = table.iter().rev().find(|e| e.mount_point.as_os_str() == mp) else {
                continue;
            };
            assert_eq!(e.st_dev(), std::fs::metadata(mp).unwrap().dev(), "{mp}");
        }
        assert_eq!(
            table
                .iter()
                .rev()
                .find(|e| e.mount_point.as_os_str() == "/proc")
                .map(|e| classify_fs(&e.fstype)),
            Some(FsClass::Kernel)
        );
    }

    #[test]
    fn filesystem_classes() {
        for (t, c) in [
            ("btrfs", FsClass::Local),
            ("tmpfs", FsClass::Local),
            ("overlay", FsClass::Local),
            ("fuse.mergerfs", FsClass::Local),
            ("nfs4", FsClass::Network),
            ("cifs", FsClass::Network),
            ("fuse.sshfs", FsClass::Network),
            ("proc", FsClass::Kernel),
            ("cgroup2", FsClass::Kernel),
        ] {
            assert_eq!(classify_fs(t), c, "{t}");
        }
    }
}
