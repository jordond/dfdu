use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{AtFlags, CWD, Mode, OFlags, StatxFlags};
use rustix::ioctl::{Getter, Setter, ioctl, opcode};

use crate::SubvolInfo;

mod generations;
pub use generations::{BtrfsChanges, btrfs_changes_since, btrfs_position};

const BTRFS_SUPER_MAGIC: i64 = 0x9123_683E;
/// Inode number of every subvolume root (`BTRFS_FIRST_FREE_OBJECTID`).
pub const BTRFS_SUBVOL_ROOT_INO: u64 = 256;
const BTRFS_IOCTL_MAGIC: u8 = 0x94;
/// `BTRFS_ROOT_SUBVOL_RDONLY` in the root item flags.
const ROOT_SUBVOL_RDONLY: u64 = 1;

fn errno(e: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(e.raw_os_error())
}

/// Mount id of `path` itself (symlinks are not followed, automounts are not triggered).
/// `Ok(None)` when the kernel does not report one (before Linux 5.8).
pub fn mount_id(path: &Path) -> io::Result<Option<u64>> {
    let flags = AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT;
    statx_mount_id(rustix::fs::statx(CWD, path, flags, StatxFlags::MNT_ID))
}

/// Mount id of an open file or directory.
pub fn mount_id_fd(fd: impl AsFd) -> io::Result<Option<u64>> {
    statx_mount_id(rustix::fs::statx(
        fd,
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::MNT_ID,
    ))
}

fn statx_mount_id(r: rustix::io::Result<rustix::fs::Statx>) -> io::Result<Option<u64>> {
    match r {
        Ok(st) => Ok((st.stx_mask & StatxFlags::MNT_ID.bits() != 0).then_some(st.stx_mnt_id)),
        Err(rustix::io::Errno::NOSYS) => Ok(None),
        Err(e) => Err(errno(e)),
    }
}

pub fn is_btrfs_fd(fd: impl AsFd) -> io::Result<bool> {
    let st = rustix::fs::fstatfs(fd).map_err(errno)?;
    Ok(st.f_type as i64 == BTRFS_SUPER_MAGIC)
}

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
        otime: a.otime.sec as i64,
    })
}

/// Subvolume information if `path` is the root directory of a btrfs subvolume, else `None`.
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

const FS_INFO: rustix::ioctl::Opcode = opcode::read::<FsInfoArgs>(BTRFS_IOCTL_MAGIC, 31);

/// `BTRFS_IOC_FS_INFO`. No privileges.
#[allow(unsafe_code)]
fn fs_info(fd: BorrowedFd<'_>) -> io::Result<FsInfoArgs> {
    // SAFETY: FS_INFO is `_IOR(0x94, 31, struct btrfs_ioctl_fs_info_args)`; `FsInfoArgs` has
    // that layout (size checked above), with no invalid bit patterns. The kernel reads `flags`
    // (zero here: request no optional fields) and writes the struct.
    unsafe {
        let mut args: FsInfoArgs = std::mem::zeroed();
        ioctl(
            fd,
            rustix::ioctl::Updater::<FS_INFO, FsInfoArgs>::new(&mut args),
        )
        .map(|()| args)
    }
    .map_err(errno)
}

/// The btrfs filesystem uuid (the same for every subvolume and mount of it). No privileges.
pub fn btrfs_fsid(fd: impl AsFd) -> io::Result<[u8; 16]> {
    Ok(fs_info(fd.as_fd())?.fsid)
}

/// One extent as FIEMAP reports it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    pub logical: u64,
    pub physical: u64,
    pub length: u64,
    pub flags: u32,
}

impl Extent {
    pub const LAST: u32 = 0x1;
    pub const UNKNOWN: u32 = 0x2;
    pub const DELALLOC: u32 = 0x4;
    /// Compressed (btrfs): `length` is the uncompressed length.
    pub const ENCODED: u32 = 0x8;
    pub const DATA_INLINE: u32 = 0x200;
    pub const UNWRITTEN: u32 = 0x800;
    pub const SHARED: u32 = 0x2000;
}

#[repr(C)]
#[derive(Copy, Clone)]
struct RawExtent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}
const _: () = assert!(std::mem::size_of::<RawExtent>() == 56);

const FIEMAP_BATCH: usize = 256;

/// `struct fiemap` with room for `FIEMAP_BATCH` extents.
#[repr(C)]
struct FiemapArgs {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [RawExtent; FIEMAP_BATCH],
}

const FS_IOC_FIEMAP: rustix::ioctl::Opcode = opcode::read_write::<[u64; 4]>(b'f', 11);

/// Reusable buffers for [`ExtentReader::fiemap`] and [`ExtentReader::file_extents`]: one per
/// thread that measures many files, so no call allocates or clears tens of kilobytes.
pub struct ExtentReader {
    fiemap: Box<FiemapArgs>,
    search: Box<SearchArgs>,
}

impl Default for ExtentReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtentReader {
    pub fn new() -> Self {
        ExtentReader {
            fiemap: Box::new(FiemapArgs {
                start: 0,
                length: 0,
                flags: 0,
                mapped_extents: 0,
                extent_count: 0,
                reserved: 0,
                extents: [RawExtent {
                    logical: 0,
                    physical: 0,
                    length: 0,
                    reserved64: [0; 2],
                    flags: 0,
                    reserved: [0; 3],
                }; FIEMAP_BATCH],
            }),
            search: SearchArgs::boxed(),
        }
    }

    /// The extent map of an open regular file (`FS_IOC_FIEMAP`), without syncing it first.
    #[allow(unsafe_code)]
    pub fn fiemap(&mut self, fd: impl AsFd) -> io::Result<Vec<Extent>> {
        let mut out = Vec::new();
        let mut start = 0u64;
        let args = &mut *self.fiemap;
        loop {
            args.start = start;
            args.length = u64::MAX - start;
            args.flags = 0;
            args.mapped_extents = 0;
            args.extent_count = FIEMAP_BATCH as u32;
            args.reserved = 0;
            // SAFETY: FS_IOC_FIEMAP is `_IOWR('f', 11, struct fiemap)`, where `struct fiemap` is
            // the 32-byte header the opcode encodes, followed by `extent_count` extents of 56
            // bytes that the kernel may write. `FiemapArgs` is that header followed by
            // `FIEMAP_BATCH` extents and `extent_count` says so, so the kernel writes only
            // inside it.
            unsafe {
                ioctl(
                    fd.as_fd(),
                    rustix::ioctl::Updater::<FS_IOC_FIEMAP, FiemapArgs>::new(args),
                )
            }
            .map_err(errno)?;
            let n = (args.mapped_extents as usize).min(FIEMAP_BATCH);
            for e in &args.extents[..n] {
                out.push(Extent {
                    logical: e.logical,
                    physical: e.physical,
                    length: e.length,
                    flags: e.flags,
                });
            }
            match args.extents[..n].last() {
                Some(e) if e.flags & Extent::LAST == 0 && n == FIEMAP_BATCH => {
                    start = e.logical + e.length;
                }
                _ => return Ok(out),
            }
        }
    }

    /// The file extent items of the open file `fd` with inode number `ino`, from its
    /// subvolume's tree (`BTRFS_IOC_TREE_SEARCH_V2`). Needs `CAP_SYS_ADMIN`.
    pub fn file_extents(&mut self, fd: impl AsFd, ino: u64) -> io::Result<Vec<FileExtent>> {
        let items = tree_search(
            &mut self.search,
            fd.as_fd(),
            0,
            (ino, ino),
            EXTENT_DATA_KEY,
            (0, u64::MAX),
        )?;
        Ok(parse_file_extents(items))
    }
}

/// The extent map of an open regular file (`FS_IOC_FIEMAP`), without syncing it first.
pub fn fiemap(fd: impl AsFd) -> io::Result<Vec<Extent>> {
    ExtentReader::new().fiemap(fd)
}

/// `struct btrfs_ioctl_search_key`.
#[repr(C)]
#[derive(Copy, Clone, Default)]
struct SearchKey {
    tree_id: u64,
    min_objectid: u64,
    max_objectid: u64,
    min_offset: u64,
    max_offset: u64,
    min_transid: u64,
    max_transid: u64,
    min_type: u32,
    max_type: u32,
    nr_items: u32,
    unused: u32,
    unused1: [u64; 4],
}
const _: () = assert!(std::mem::size_of::<SearchKey>() == 104);

/// `struct btrfs_ioctl_search_args_v2` without its flexible buffer.
#[repr(C)]
struct SearchArgsHeader {
    key: SearchKey,
    buf_size: u64,
}
const _: () = assert!(std::mem::size_of::<SearchArgsHeader>() == 112);

const SEARCH_BUF: usize = 16 * 1024;

/// The header followed by a result buffer of `N` bytes, as the kernel expects it.
#[repr(C)]
pub(crate) struct SearchArgs<const N: usize = SEARCH_BUF> {
    key: SearchKey,
    buf_size: u64,
    buf: [u8; N],
}

impl<const N: usize> SearchArgs<N> {
    pub(crate) fn boxed() -> Box<Self> {
        Box::new(SearchArgs {
            key: SearchKey::default(),
            buf_size: N as u64,
            buf: [0; N],
        })
    }
}

const TREE_SEARCH_V2: rustix::ioctl::Opcode =
    opcode::read_write::<SearchArgsHeader>(BTRFS_IOCTL_MAGIC, 17);

/// One item found by a tree search: its key and its data.
pub(crate) struct SearchItem {
    pub(crate) objectid: u64,
    pub(crate) item_type: u32,
    pub(crate) offset: u64,
    pub(crate) data: Vec<u8>,
}

/// A btrfs key: objectid, type, offset, ordered in that order.
pub(crate) type Key = (u64, u32, u64);

/// The key after `k`, `None` after the last.
fn next_key(k: Key) -> Option<Key> {
    if k.2 < u64::MAX {
        Some((k.0, k.1, k.2 + 1))
    } else if k.1 < u8::MAX as u32 {
        Some((k.0, k.1 + 1, 0))
    } else if k.0 < u64::MAX {
        Some((k.0 + 1, 0, 0))
    } else {
        None
    }
}

/// Calls `f` with every item of `tree_id` (0: the subvolume of `fd`) with a key in
/// `[min, max]` (compared as whole keys, so `min.1..=max.1` does not filter types between
/// different objectids) in a leaf of generation `min_transid` or later. Stops at `f`'s first
/// `false`. Needs `CAP_SYS_ADMIN`.
#[allow(unsafe_code)]
pub(crate) fn search<const N: usize>(
    args: &mut SearchArgs<N>,
    fd: BorrowedFd<'_>,
    tree_id: u64,
    min: Key,
    max: Key,
    min_transid: u64,
    mut f: impl FnMut(SearchItem) -> bool,
) -> io::Result<()> {
    let mut from = min;
    loop {
        args.key = SearchKey {
            tree_id,
            min_objectid: from.0,
            max_objectid: max.0,
            min_type: from.1,
            max_type: max.1,
            min_offset: from.2,
            max_offset: max.2,
            min_transid,
            max_transid: u64::MAX,
            nr_items: u32::MAX,
            ..Default::default()
        };
        args.buf_size = N as u64;
        // SAFETY: TREE_SEARCH_V2 is `_IOWR(0x94, 17, struct btrfs_ioctl_search_args_v2)`, a
        // 112-byte header (`SearchArgsHeader`, size checked) followed by `buf_size` bytes the
        // kernel may write. `SearchArgs` is that header followed by `N` bytes and `buf_size`
        // says so.
        unsafe {
            ioctl(
                fd,
                rustix::ioctl::Updater::<TREE_SEARCH_V2, SearchArgs<N>>::new(args),
            )
        }
        .map_err(errno)?;
        let n = args.key.nr_items;
        if n == 0 {
            return Ok(());
        }
        // Items: a 32-byte header (transid, objectid, offset: u64; type, len: u32), then data.
        let buf = &args.buf[..];
        let mut at = 0usize;
        let mut last = from;
        for _ in 0..n {
            let h = buf.get(at..at + 32).ok_or(io::ErrorKind::InvalidData)?;
            let u64_at = |i: usize| u64::from_ne_bytes(h[i..i + 8].try_into().expect("8 bytes"));
            let (objectid, offset) = (u64_at(8), u64_at(16));
            let item_type = u32::from_ne_bytes(h[24..28].try_into().expect("4 bytes"));
            let len = u32::from_ne_bytes(h[28..32].try_into().expect("4 bytes")) as usize;
            let data = buf
                .get(at + 32..at + 32 + len)
                .ok_or(io::ErrorKind::InvalidData)?
                .to_vec();
            at += 32 + len;
            last = (objectid, item_type, offset);
            let more = f(SearchItem {
                objectid,
                item_type,
                offset,
                data,
            });
            if !more {
                return Ok(());
            }
        }
        match next_key(last) {
            Some(k) if k <= max => from = k,
            _ => return Ok(()),
        }
    }
}

/// All items of type `item_type` with objectid and offset in the given ranges, of `tree_id`
/// (0: the subvolume of `fd`). Needs `CAP_SYS_ADMIN`.
fn tree_search(
    args: &mut SearchArgs,
    fd: BorrowedFd<'_>,
    tree_id: u64,
    objectid: (u64, u64),
    item_type: u32,
    offset: (u64, u64),
) -> io::Result<Vec<SearchItem>> {
    let mut out = Vec::new();
    search(
        args,
        fd,
        tree_id,
        (objectid.0, item_type, offset.0),
        (objectid.1, item_type, offset.1),
        0,
        |it| {
            if it.item_type == item_type && (offset.0..=offset.1).contains(&it.offset) {
                out.push(it);
            }
            true
        },
    )?;
    Ok(out)
}

/// One file extent item of a btrfs file (`struct btrfs_file_extent_item`, which is packed).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FileExtent {
    /// Offset in the file.
    pub file_offset: u64,
    /// 0 none, 1 zlib, 2 lzo, 3 zstd.
    pub compression: u8,
    pub kind: FileExtentKind,
    /// The extent on disk (logical address in the filesystem); 0 for a hole.
    pub disk_bytenr: u64,
    /// Bytes the extent takes on disk (compressed size for compressed extents).
    pub disk_num_bytes: u64,
    /// Bytes of the file this item covers.
    pub num_bytes: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileExtentKind {
    /// Data stored in the metadata item; `disk_num_bytes` is its stored size.
    Inline,
    Regular,
    Prealloc,
}

const EXTENT_DATA_KEY: u32 = 108;

/// The file extent items of the open file `fd` with inode number `ino`, from its subvolume's
/// tree (`BTRFS_IOC_TREE_SEARCH_V2`). Needs `CAP_SYS_ADMIN`.
pub fn file_extents(fd: impl AsFd, ino: u64) -> io::Result<Vec<FileExtent>> {
    ExtentReader::new().file_extents(fd, ino)
}

fn parse_file_extents(items: Vec<SearchItem>) -> Vec<FileExtent> {
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let d = &it.data;
        if it.item_type != EXTENT_DATA_KEY || d.len() < 21 {
            continue;
        }
        let le = |i: usize| u64::from_le_bytes(d[i..i + 8].try_into().expect("8 bytes"));
        let compression = d[16];
        let (kind, disk_bytenr, disk_num_bytes, num_bytes) = match d[20] {
            0 => (FileExtentKind::Inline, 0, (d.len() - 21) as u64, le(8)),
            t @ (1 | 2) if d.len() >= 53 => (
                if t == 1 {
                    FileExtentKind::Regular
                } else {
                    FileExtentKind::Prealloc
                },
                le(21),
                le(29),
                le(45),
            ),
            _ => continue,
        };
        out.push(FileExtent {
            file_offset: it.offset,
            compression,
            kind,
            disk_bytenr,
            disk_num_bytes,
            num_bytes,
        });
    }
    out
}

const QUOTA_TREE_OBJECTID: u64 = 8;
const QGROUP_INFO_KEY: u32 = 242;

/// Referenced and exclusive bytes of a subvolume's level-0 quota group, when quotas are
/// enabled (`None` otherwise). Needs `CAP_SYS_ADMIN`. The numbers are btrfs's own and can lag
/// behind until a quota rescan.
pub fn qgroup_usage(fd: impl AsFd, subvol_id: u64) -> io::Result<Option<(u64, u64)>> {
    let items = match tree_search(
        &mut ExtentReader::new().search,
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
        .map(|i| {
            let le = |o: usize| u64::from_le_bytes(i.data[o..o + 8].try_into().expect("8 bytes"));
            (le(8), le(24))
        }))
}

/// Data allocation profile of a btrfs filesystem.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    Single,
    Dup,
    Raid0,
    Raid1,
    Raid1c3,
    Raid1c4,
    Raid10,
    Raid5,
    Raid6,
}

impl Profile {
    /// Raw bytes stored per byte of data, given the number of devices.
    pub fn raw_ratio(self, devices: u64) -> f64 {
        let n = devices.max(1) as f64;
        match self {
            Profile::Single | Profile::Raid0 => 1.0,
            Profile::Dup | Profile::Raid1 | Profile::Raid10 => 2.0,
            Profile::Raid1c3 => 3.0,
            Profile::Raid1c4 => 4.0,
            Profile::Raid5 => n / (n - 1.0).max(1.0),
            Profile::Raid6 => n / (n - 2.0).max(1.0),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Single => "single",
            Profile::Dup => "DUP",
            Profile::Raid0 => "RAID0",
            Profile::Raid1 => "RAID1",
            Profile::Raid1c3 => "RAID1C3",
            Profile::Raid1c4 => "RAID1C4",
            Profile::Raid10 => "RAID10",
            Profile::Raid5 => "RAID5",
            Profile::Raid6 => "RAID6",
        }
    }
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
#[allow(unsafe_code)]
pub fn data_profile(fd: impl AsFd) -> io::Result<(Profile, u64)> {
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
    const DATA: u64 = 1;
    let n = (args.total_spaces as usize).min(SPACE_SLOTS);
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

/// Whether this process may do what root may: delete read-only snapshots, read extent trees.
pub fn is_privileged() -> bool {
    rustix::process::geteuid().is_root()
}

/// The mount table of this process's mount namespace.
pub fn mount_table() -> io::Result<Vec<crate::MountEntry>> {
    Ok(crate::parse_mountinfo(&std::fs::read(
        "/proc/self/mountinfo",
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_directory_is_not_a_subvolume() {
        let d = tempfile::tempdir().unwrap();
        if is_btrfs(d.path()).unwrap() {
            // The temp dir itself is on btrfs; the btrfs tests cover this case.
            return;
        }
        assert_eq!(subvol_info(d.path()).unwrap(), None);
        let fd = open_dir(CWD, d.path()).unwrap();
        let e = subvol_info_fd(&fd).unwrap_err();
        assert_eq!(
            e.raw_os_error(),
            Some(rustix::io::Errno::NOTTY.raw_os_error())
        );
    }

    #[test]
    fn mount_id_is_shared_within_a_filesystem() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (a, b) = (
            mount_id(d.path()).unwrap(),
            mount_id(&d.path().join("sub")).unwrap(),
        );
        assert!(a.is_some(), "kernel reports mount ids");
        assert_eq!(a, b);
        // /proc is always its own mount.
        assert_ne!(mount_id(Path::new("/proc")).unwrap(), a);
    }

    #[test]
    fn destroy_rejects_bad_names() {
        let d = tempfile::tempdir().unwrap();
        let dir = open_dir(CWD, d.path()).unwrap();
        for bad in ["", "a/b", "a\0b"] {
            let e = destroy_subvolume_at(&dir, OsStr::new(bad)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }
}
