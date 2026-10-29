//! Extent maps of files: FIEMAP, which most filesystems answer, and btrfs's own file extent
//! items, read from the subvolume's tree.

use std::io;
use std::os::fd::AsFd;

use rustix::ioctl::{ioctl, opcode};

use super::search::{SearchArgs, SearchItem, tree_search};
use super::{errno, le_u64};

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

const FIEMAP_BATCH: usize = FIEMAP_BATCH_U32 as usize;
const FIEMAP_BATCH_U32: u32 = 256;

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
    #[must_use]
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
    ///
    /// # Errors
    ///
    /// The error the ioctl reports (`EOPNOTSUPP` where the filesystem has no extent maps).
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
            args.extent_count = FIEMAP_BATCH_U32;
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
    ///
    /// # Errors
    ///
    /// The error the ioctl reports (`EPERM` without the capability), or `InvalidData` for a
    /// result the kernel cut short.
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
///
/// # Errors
///
/// As [`ExtentReader::fiemap`].
pub fn fiemap(fd: impl AsFd) -> io::Result<Vec<Extent>> {
    ExtentReader::new().fiemap(fd)
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
///
/// # Errors
///
/// As [`ExtentReader::file_extents`].
pub fn file_extents(fd: impl AsFd, ino: u64) -> io::Result<Vec<FileExtent>> {
    ExtentReader::new().file_extents(fd, ino)
}

fn parse_file_extents(items: Vec<SearchItem>) -> Vec<FileExtent> {
    items
        .into_iter()
        .filter_map(|it| parse_file_extent(&it))
        .collect()
}

/// A file extent item, `None` for another item or one cut short.
fn parse_file_extent(it: &SearchItem) -> Option<FileExtent> {
    let d = &it.data;
    if it.item_type != EXTENT_DATA_KEY || d.len() < 21 {
        return None;
    }
    let compression = d[16];
    let (kind, disk_bytenr, disk_num_bytes, num_bytes) = match d[20] {
        0 => (
            FileExtentKind::Inline,
            0,
            (d.len() - 21) as u64,
            le_u64(d, 8)?,
        ),
        t @ (1 | 2) => (
            if t == 1 {
                FileExtentKind::Regular
            } else {
                FileExtentKind::Prealloc
            },
            le_u64(d, 21)?,
            le_u64(d, 29)?,
            le_u64(d, 45)?,
        ),
        _ => return None,
    };
    Some(FileExtent {
        file_offset: it.offset,
        compression,
        kind,
        disk_bytenr,
        disk_num_bytes,
        num_bytes,
    })
}
