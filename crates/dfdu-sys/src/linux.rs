//! The Linux calls: `statx` mount ids, the btrfs ioctls (subvolume info and destroy, filesystem
//! info, qgroup usage, the data profile) and FIEMAP extents, each ioctl argument struct checked
//! against the kernel's size at compile time; and the mount table from `/proc`.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{AtFlags, CWD, Mode, OFlags, StatxFlags};
use rustix::ioctl::{Getter, Setter, ioctl, opcode};

use crate::{Profile, SubvolInfo};

mod extents;
mod generations;
mod search;
pub use extents::{Extent, ExtentReader, FileExtent, FileExtentKind, fiemap, file_extents};
pub use generations::{BtrfsChanges, btrfs_changes_since, btrfs_position};
use search::{SearchArgs, tree_search};

const BTRFS_SUPER_MAGIC: i64 = 0x9123_683E;
/// Inode number of every subvolume root (`BTRFS_FIRST_FREE_OBJECTID`).
pub const BTRFS_SUBVOL_ROOT_INO: u64 = 256;
const BTRFS_IOCTL_MAGIC: u8 = 0x94;
/// `BTRFS_ROOT_SUBVOL_RDONLY` in the root item flags.
const ROOT_SUBVOL_RDONLY: u64 = 1;

fn errno(e: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(e.raw_os_error())
}

/// The native-endian `u32` at byte `i` of `b`, if `b` holds it.
fn ne_u32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// The native-endian `u64` at byte `i` of `b`, if `b` holds it.
fn ne_u64(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// The little-endian `u32` at byte `i` of `b`, if `b` holds it.
fn le_u32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// The little-endian `u64` at byte `i` of `b`, if `b` holds it.
fn le_u64(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// Mount id of `path` itself (symlinks are not followed, automounts are not triggered).
/// `Ok(None)` when the kernel does not report one (before Linux 5.8).
///
/// # Errors
///
/// The error `statx` reports, such as `ENOENT`.
pub fn mount_id(path: &Path) -> io::Result<Option<u64>> {
    let flags = AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT;
    statx_mount_id(&rustix::fs::statx(CWD, path, flags, StatxFlags::MNT_ID))
}

/// Mount id of an open file or directory.
///
/// # Errors
///
/// The error `statx` reports.
pub fn mount_id_fd(fd: impl AsFd) -> io::Result<Option<u64>> {
    statx_mount_id(&rustix::fs::statx(
        fd,
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::MNT_ID,
    ))
}

fn statx_mount_id(r: &rustix::io::Result<rustix::fs::Statx>) -> io::Result<Option<u64>> {
    match r {
        Ok(st) => Ok((st.stx_mask & StatxFlags::MNT_ID.bits() != 0).then_some(st.stx_mnt_id)),
        Err(rustix::io::Errno::NOSYS) => Ok(None),
        Err(e) => Err(errno(*e)),
    }
}

/// Whether the open file or directory `fd` is on btrfs.
///
/// # Errors
///
/// The error `fstatfs` reports.
pub fn is_btrfs_fd(fd: impl AsFd) -> io::Result<bool> {
    let st = rustix::fs::fstatfs(fd).map_err(errno)?;
    Ok(st.f_type as i64 == BTRFS_SUPER_MAGIC)
}

/// Whether the directory `path` is on btrfs.
///
/// # Errors
///
/// When `path` cannot be opened as a directory (a symlink is not followed), or `fstatfs` fails.
pub fn is_btrfs(path: &Path) -> io::Result<bool> {
    is_btrfs_fd(open_dir(CWD, path)?)
}

fn open_dir(dir: impl AsFd, path: &Path) -> io::Result<OwnedFd> {
    rustix::fs::openat(
        dir,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno)
}

#[repr(C)]
#[derive(Copy, Clone)]
struct IoctlTimespec {
    sec: u64,
    nsec: u32,
}

/// `struct btrfs_ioctl_get_subvol_info_args`.
#[repr(C)]
#[derive(Copy, Clone)]
struct GetSubvolInfoArgs {
    treeid: u64,
    name: [u8; 256],
    parent_id: u64,
    dirid: u64,
    generation: u64,
    flags: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    received_uuid: [u8; 16],
    ctransid: u64,
    otransid: u64,
    stransid: u64,
    rtransid: u64,
    ctime: IoctlTimespec,
    otime: IoctlTimespec,
    stime: IoctlTimespec,
    rtime: IoctlTimespec,
    reserved: [u64; 8],
}
const _: () = assert!(std::mem::size_of::<GetSubvolInfoArgs>() == 504);

const GET_SUBVOL_INFO: rustix::ioctl::Opcode =
    opcode::read::<GetSubvolInfoArgs>(BTRFS_IOCTL_MAGIC, 60);

/// `BTRFS_IOC_GET_SUBVOL_INFO` of the subvolume holding `fd` (no privileges needed, Linux
/// 4.18). Fails with `ENOTTY` on other filesystems.
#[allow(unsafe_code)]
fn subvol_args(fd: impl AsFd) -> io::Result<GetSubvolInfoArgs> {
    // SAFETY: GET_SUBVOL_INFO is `_IOR(0x94, 60, struct btrfs_ioctl_get_subvol_info_args)`;
    // `GetSubvolInfoArgs` has that layout (size checked above) and every bit pattern is a valid
    // value of it, so the kernel writing it in full produces a valid value.
    unsafe {
        ioctl(
            fd.as_fd(),
            Getter::<GET_SUBVOL_INFO, GetSubvolInfoArgs>::new(),
        )
    }
    .map_err(errno)
}

/// Subvolume information for an open subvolume root directory. Fails with `ENOTTY` on other
/// filesystems.
fn subvol_info_fd(dir: impl AsFd) -> io::Result<SubvolInfo> {
    let a = subvol_args(dir)?;
    let name_len = a.name.iter().position(|&b| b == 0).unwrap_or(a.name.len());
    Ok(SubvolInfo {
        id: a.treeid,
        parent_id: a.parent_id,
        name: a.name[..name_len].to_vec(),
        uuid: a.uuid,
        parent_uuid: (a.parent_uuid != [0; 16]).then_some(a.parent_uuid),
        read_only: a.flags & ROOT_SUBVOL_RDONLY != 0,
        otime: a.otime.sec.cast_signed(),
    })
}

/// Subvolume information if `path` is the root directory of a btrfs subvolume, else `None`.
///
/// # Errors
///
/// When `path` cannot be opened as a directory, or `fstatfs`, `fstat` or the ioctl fails.
pub fn subvol_info(path: &Path) -> io::Result<Option<SubvolInfo>> {
    let fd = open_dir(CWD, path)?;
    if !is_btrfs_fd(&fd)? {
        return Ok(None);
    }
    if rustix::fs::fstat(&fd).map_err(errno)?.st_ino != BTRFS_SUBVOL_ROOT_INO {
        return Ok(None);
    }
    subvol_info_fd(&fd).map(Some)
}

/// `struct btrfs_ioctl_vol_args`.
#[repr(C)]
struct VolArgs {
    fd: i64,
    name: [u8; 4088],
}
const _: () = assert!(std::mem::size_of::<VolArgs>() == 4096);

const SNAP_DESTROY: rustix::ioctl::Opcode = opcode::write::<VolArgs>(BTRFS_IOCTL_MAGIC, 15);

/// Deletes the subvolume `name` in the directory `parent` (`BTRFS_IOC_SNAP_DESTROY`). Root may
/// delete any subvolume; other users only read-write subvolumes they own, and only on
/// filesystems mounted with `user_subvol_rm_allowed`. The space is freed later by the cleaner.
///
/// # Errors
///
/// `InvalidInput` when `name` is empty, too long or holds `/` or NUL, else the error the ioctl
/// reports (`EPERM` without the right to delete it, `ENOTTY` off btrfs).
#[allow(unsafe_code)]
pub fn destroy_subvolume_at(parent: impl AsFd, name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() >= 4088 || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut args = VolArgs {
        fd: 0,
        name: [0; 4088],
    };
    args.name[..bytes.len()].copy_from_slice(bytes);
    // SAFETY: SNAP_DESTROY is `_IOW(0x94, 15, struct btrfs_ioctl_vol_args)`; `VolArgs` has that
    // layout (size checked above) and the name is NUL-terminated within it.
    unsafe { ioctl(parent.as_fd(), Setter::<SNAP_DESTROY, VolArgs>::new(args)) }.map_err(errno)
}

const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
const BCACHEFS_SUPER_MAGIC: i64 = 0xca45_1a4e;

/// Filesystems where files can share extents (reflinks, snapshots, deduplication), so FIEMAP's
/// `SHARED` flag is worth reading.
///
/// # Errors
///
/// The error `statfs` reports.
pub fn shares_extents(path: &Path) -> io::Result<bool> {
    let st = rustix::fs::statfs(path).map_err(errno)?;
    let t = st.f_type as i64;
    Ok(t == BTRFS_SUPER_MAGIC || t == XFS_SUPER_MAGIC || t == BCACHEFS_SUPER_MAGIC)
}

/// `struct btrfs_ioctl_fs_info_args`.
#[repr(C)]
#[derive(Copy, Clone)]
struct FsInfoArgs {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; 16],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    csum_type: u16,
    csum_size: u16,
    flags: u64,
    generation: u64,
    metadata_uuid: [u8; 16],
    reserved: [u8; 944],
}
const _: () = assert!(std::mem::size_of::<FsInfoArgs>() == 1024);

impl FsInfoArgs {
    /// All fields zero: `flags` 0 asks for no optional fields.
    fn zeroed() -> Self {
        FsInfoArgs {
            max_id: 0,
            num_devices: 0,
            fsid: [0; 16],
            nodesize: 0,
            sectorsize: 0,
            clone_alignment: 0,
            csum_type: 0,
            csum_size: 0,
            flags: 0,
            generation: 0,
            metadata_uuid: [0; 16],
            reserved: [0; 944],
        }
    }
}

const FS_INFO: rustix::ioctl::Opcode = opcode::read::<FsInfoArgs>(BTRFS_IOCTL_MAGIC, 31);

/// `BTRFS_IOC_FS_INFO`. No privileges.
#[allow(unsafe_code)]
fn fs_info(fd: BorrowedFd<'_>) -> io::Result<FsInfoArgs> {
    let mut args = FsInfoArgs::zeroed();
    // SAFETY: FS_INFO is `_IOR(0x94, 31, struct btrfs_ioctl_fs_info_args)`; `FsInfoArgs` has
    // that layout (size checked above), with no invalid bit patterns. The kernel reads `flags`
    // (zero here: request no optional fields) and writes the struct.
    unsafe {
        ioctl(
            fd,
            rustix::ioctl::Updater::<FS_INFO, FsInfoArgs>::new(&mut args),
        )
    }
    .map(|()| args)
    .map_err(errno)
}

/// The btrfs filesystem uuid (the same for every subvolume and mount of it). No privileges.
///
/// # Errors
///
/// The error the ioctl reports (`ENOTTY` off btrfs).
pub fn btrfs_fsid(fd: impl AsFd) -> io::Result<[u8; 16]> {
    Ok(fs_info(fd.as_fd())?.fsid)
}

const QUOTA_TREE_OBJECTID: u64 = 8;
const QGROUP_INFO_KEY: u32 = 242;

/// Referenced and exclusive bytes of a subvolume's level-0 quota group, when quotas are
/// enabled (`None` otherwise). Needs `CAP_SYS_ADMIN`. The numbers are btrfs's own and can lag
/// behind until a quota rescan.
///
/// # Errors
///
/// The error the ioctl reports (`EPERM` without the capability), or `InvalidData` for a
/// result the kernel cut short.
pub fn qgroup_usage(fd: impl AsFd, subvol_id: u64) -> io::Result<Option<(u64, u64)>> {
    let items = match tree_search(
        &mut SearchArgs::boxed(),
        fd.as_fd(),
        QUOTA_TREE_OBJECTID,
        (0, 0),
        QGROUP_INFO_KEY,
        (subvol_id, subvol_id),
    ) {
        Ok(i) => i,
        // No quota tree: quotas are off.
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NOENT.raw_os_error()) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    // `struct btrfs_qgroup_info_item`: generation, rfer, rfer_cmpr, excl, excl_cmpr (u64 LE).
    Ok(items
        .iter()
        .find(|i| i.objectid == 0 && i.offset == subvol_id && i.data.len() >= 40)
        .and_then(|i| Some((le_u64(&i.data, 8)?, le_u64(&i.data, 24)?))))
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct SpaceInfo {
    flags: u64,
    total_bytes: u64,
    used_bytes: u64,
}

const SPACE_SLOTS: usize = 32;

#[repr(C)]
struct SpaceArgs {
    space_slots: u64,
    total_spaces: u64,
    spaces: [SpaceInfo; SPACE_SLOTS],
}

/// `struct btrfs_ioctl_space_args` without its flexible array.
#[repr(C)]
struct SpaceArgsHeader {
    space_slots: u64,
    total_spaces: u64,
}

const SPACE_INFO: rustix::ioctl::Opcode =
    opcode::read_write::<SpaceArgsHeader>(BTRFS_IOCTL_MAGIC, 20);

/// The data profile of the btrfs filesystem of `fd` and its number of devices
/// (`BTRFS_IOC_SPACE_INFO`, `BTRFS_IOC_FS_INFO`; no privileges). With several data profiles
/// (during a conversion) the one with the most data is reported.
///
/// # Errors
///
/// The error the ioctls report (`ENOTTY` off btrfs), or `NotFound` when no data space is
/// reported.
#[allow(unsafe_code)]
pub fn data_profile(fd: impl AsFd) -> io::Result<(Profile, u64)> {
    const DATA: u64 = 1;
    let mut args = Box::new(SpaceArgs {
        space_slots: SPACE_SLOTS as u64,
        total_spaces: 0,
        spaces: [SpaceInfo::default(); SPACE_SLOTS],
    });
    // SAFETY: SPACE_INFO is `_IOWR(0x94, 20, struct btrfs_ioctl_space_args)`, a 16-byte header
    // followed by `space_slots` entries of 24 bytes the kernel may write; `SpaceArgs` has that
    // layout with `SPACE_SLOTS` entries and `space_slots` says so.
    unsafe {
        ioctl(
            fd.as_fd(),
            rustix::ioctl::Updater::<SPACE_INFO, SpaceArgs>::new(&mut args),
        )
    }
    .map_err(errno)?;
    let n = usize::try_from(args.total_spaces).map_or(SPACE_SLOTS, |n| n.min(SPACE_SLOTS));
    let data = args.spaces[..n]
        .iter()
        .filter(|s| s.flags & DATA != 0)
        .max_by_key(|s| s.used_bytes)
        .ok_or(io::ErrorKind::NotFound)?;
    let f = data.flags;
    let profile = if f & (1 << 3) != 0 {
        Profile::Raid0
    } else if f & (1 << 4) != 0 {
        Profile::Raid1
    } else if f & (1 << 5) != 0 {
        Profile::Dup
    } else if f & (1 << 6) != 0 {
        Profile::Raid10
    } else if f & (1 << 7) != 0 {
        Profile::Raid5
    } else if f & (1 << 8) != 0 {
        Profile::Raid6
    } else if f & (1 << 9) != 0 {
        Profile::Raid1c3
    } else if f & (1 << 10) != 0 {
        Profile::Raid1c4
    } else {
        Profile::Single
    };
    Ok((profile, fs_info(fd.as_fd())?.num_devices))
}

/// The mount table of this process's mount namespace.
///
/// # Errors
///
/// When `/proc/self/mountinfo` cannot be read.
pub fn mount_table() -> io::Result<Vec<crate::MountEntry>> {
    Ok(crate::parse_mountinfo(&std::fs::read(
        "/proc/self/mountinfo",
    )?))
}

#[cfg(test)]
mod tests;
