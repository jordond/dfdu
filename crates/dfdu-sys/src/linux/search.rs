//! btrfs tree searches (`BTRFS_IOC_TREE_SEARCH_V2`): items of a tree by key range.

use std::io;
use std::os::fd::BorrowedFd;

use rustix::ioctl::{ioctl, opcode};

use super::{BTRFS_IOCTL_MAGIC, errno, ne_u32, ne_u64};

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
    } else if k.1 < u32::from(u8::MAX) {
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
            let bad = || io::Error::from(io::ErrorKind::InvalidData);
            let objectid = ne_u64(buf, at + 8).ok_or_else(bad)?;
            let offset = ne_u64(buf, at + 16).ok_or_else(bad)?;
            let item_type = ne_u32(buf, at + 24).ok_or_else(bad)?;
            let len = ne_u32(buf, at + 28).ok_or_else(bad)? as usize;
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
pub(crate) fn tree_search(
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

#[cfg(test)]
mod tests;
