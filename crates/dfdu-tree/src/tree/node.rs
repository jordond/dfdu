//! One slot of the tree and how its fields are packed: the allocation in 512-byte blocks, the
//! error in a byte, the device as an index, with side tables for what does not fit.

use super::{Ix, Tree};
use crate::{ErrKind, Kind};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeFlags(pub(super) u16);

impl NodeFlags {
    pub const HARDLINK: u16 = 1;
    /// Hard link whose bytes are counted (exactly one per inode and scope).
    pub const CHARGED: u16 = 2;
    /// Root of a detached subtree.
    pub const DETACHED: u16 = 4;
    pub const FREE: u16 = 8;
    /// Root of the visible tree.
    pub const VISIBLE_ROOT: u16 = 16;
    /// In a delete plan that has not finished.
    pub const DELETING: u16 = 32;
    /// Directory whose mount differs from its parent's (counted in `Totals::mounts`).
    pub const MOUNT_ROOT: u16 = 64;
    /// btrfs subvolume root; its record is in the model's subvolume table.
    pub const SUBVOL: u16 = 128;
    /// Read-only btrfs subvolume root (counted in `Totals::ro_subvols`).
    pub const RO_SUBVOL: u16 = 256;
    /// Mount of something already scanned elsewhere in the job; not descended into.
    pub const DUP_MOUNT: u16 = 512;
    /// File whose extents were measured: its own disk usage is its unshared bytes instead of its
    /// allocation.
    pub const MEASURED: u16 = 1024;
    /// Measured file with compressed extents whose on-disk size is not known (an upper bound).
    pub const COMPRESSED: u16 = 2048;
    /// Mount of a network filesystem, not descended into (scanned only when asked for).
    pub const NETWORK: u16 = 4096;
    /// Mount of a kernel filesystem (`/proc`, `/sys`), never descended into.
    pub const KERNEL: u16 = 8192;
    /// Top of a protected subtree (`dfdu-rules` protected paths): nothing in it is offered for
    /// deletion, except below an [`NodeFlags::EXCEPTED`] node.
    pub const PROTECTED: u16 = 16384;
    /// Top of a subtree an exception lifts out of the protected subtree around it.
    pub const EXCEPTED: u16 = 32768;

    #[must_use]
    pub fn has(self, f: u16) -> bool {
        self.0 & f != 0
    }
    pub(super) fn set(&mut self, f: u16, on: bool) {
        if on { self.0 |= f } else { self.0 &= !f }
    }
}

/// One slot, kept to 56 bytes: memory per entry decides how large a tree fits (the target is
/// under 100 MB for 1M entries). Fields most entries do not need live in side tables.
#[derive(Clone, Debug)]
pub(super) struct Node {
    /// Handle of the name in [`Tree`]'s name arena.
    pub(super) name: u64,
    pub(super) apparent: u64,
    pub(super) mtime: i64,
    pub(super) ino: u64,
    /// Allocation in 512-byte blocks, packed by [`pack_alloc`]; `ALLOC_BIG` when it is in
    /// `Tree::big_allocs`.
    pub(super) blocks: u32,
    pub(super) generation: u32,
    pub(super) parent: u32,
    pub(super) dir: u32,
    /// Index into [`Tree::devs`].
    pub(super) dev: u32,
    pub(super) flags: NodeFlags,
    /// The node's own error, packed by [`pack_err`]; 0 for none.
    pub(super) error: u8,
    pub(super) kind: Kind,
}

const _: () = assert!(std::mem::size_of::<Node>() <= 56);

/// [`Node::blocks`] of an allocation that is not a whole number of 512-byte blocks below
/// 2 TiB. Unix backends report whole blocks, and Windows whole clusters, except NTFS files whose
/// data is inside their MFT record (up to about 700 bytes), whose allocation is a multiple of 8
/// and goes to the side map; files of 2 TiB or more are rare.
const ALLOC_BIG: u32 = u32::MAX;

/// Packs an allocation into a node's block count; `None` when it needs `Tree::big_allocs`.
fn pack_alloc(alloc: u64) -> Option<u32> {
    if !alloc.is_multiple_of(512) {
        return None;
    }
    u32::try_from(alloc / 512).ok().filter(|&b| b != ALLOC_BIG)
}

/// Error codes in [`Node::error`]. OS error numbers are stored above `ERR_OS`; those that do
/// not fit (above 249, which Linux and macOS never use) go to `Tree::big_errors` under
/// `ERR_BIG`.
const ERR_PERMISSION: u8 = 1;
const ERR_NOT_FOUND: u8 = 2;
const ERR_STALLED: u8 = 3;
const ERR_OTHER: u8 = 4;
const ERR_OS: u8 = 5;
const ERR_BIG: u8 = u8::MAX;

/// Packs an error into a node's code; `None` when it needs `Tree::big_errors`.
fn pack_err(e: ErrKind) -> Option<u8> {
    Some(match e {
        ErrKind::PermissionDenied => ERR_PERMISSION,
        ErrKind::NotFound => ERR_NOT_FOUND,
        ErrKind::Stalled => ERR_STALLED,
        ErrKind::Other => ERR_OTHER,
        ErrKind::Os(code) => {
            let c = u8::try_from(code).ok()?.checked_add(ERR_OS)?;
            if c == ERR_BIG {
                return None;
            }
            c
        }
    })
}

impl Tree {
    pub(super) fn alloc_of(&self, i: u32, n: &Node) -> u64 {
        match n.blocks {
            ALLOC_BIG => self.big_allocs[&i],
            b => u64::from(b) * 512,
        }
    }

    pub(super) fn set_alloc(&mut self, i: u32, alloc: u64) {
        self.nodes[i as usize].blocks = if let Some(b) = pack_alloc(alloc) {
            if !self.big_allocs.is_empty() {
                self.big_allocs.remove(&i);
            }
            b
        } else {
            self.big_allocs.insert(i, alloc);
            ALLOC_BIG
        };
    }

    #[must_use]
    pub fn error(&self, ix: Ix) -> Option<ErrKind> {
        Some(match self.nodes[ix.0 as usize].error {
            0 => return None,
            ERR_PERMISSION => ErrKind::PermissionDenied,
            ERR_NOT_FOUND => ErrKind::NotFound,
            ERR_STALLED => ErrKind::Stalled,
            ERR_OTHER => ErrKind::Other,
            ERR_BIG => ErrKind::Os(self.big_errors[&ix.0]),
            c => ErrKind::Os(i32::from(c - ERR_OS)),
        })
    }

    pub(super) fn set_error_code(&mut self, i: u32, err: Option<ErrKind>) {
        self.big_errors.remove(&i);
        self.nodes[i as usize].error = match err {
            None => 0,
            Some(e) => pack_err(e).unwrap_or_else(|| {
                let ErrKind::Os(code) = e else {
                    unreachable!("only OS error numbers are too large")
                };
                self.big_errors.insert(i, code);
                ERR_BIG
            }),
        };
    }

    pub(super) fn dev_index(&mut self, dev: u64) -> u32 {
        if let Some((last, d)) = self.last_dev
            && last == dev
        {
            return d;
        }
        let d = if let Some(&d) = self.dev_index.get(&dev) {
            d
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "every device is some node's, and nodes are numbered by u32"
            )]
            let d = self.devs.len() as u32;
            self.devs.push(dev);
            self.dev_index.insert(dev, d);
            d
        };
        self.last_dev = Some((dev, d));
        d
    }
}

#[cfg(test)]
mod tests;
