//! btrfs as a change journal: every tree block records the transaction (generation) that last
//! wrote it, and every inode item the one that last changed the inode. A subvolume's
//! generation is its position; the directories changed since are those whose inode changed
//! after it, and the parents of the files whose inode did, found by a tree search that skips
//! every block older than the position (as `btrfs subvolume find-new` does).
//!
//! Both need `CAP_SYS_ADMIN` (`BTRFS_IOC_TREE_SEARCH_V2`, `BTRFS_IOC_INO_LOOKUP`): without it
//! there is no position, and resumes rescan everything. Other subvolumes have trees of their
//! own: changes inside a nested subvolume are not seen here, so a resume rescans those whole.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::CWD;
use rustix::ioctl::{ioctl, opcode};

use super::{
    BTRFS_IOCTL_MAGIC, BTRFS_SUBVOL_ROOT_INO, Key, SearchArgs, errno, is_btrfs_fd, open_dir,
    search, subvol_args,
};

const INODE_ITEM_KEY: u32 = 1;
const INODE_REF_KEY: u32 = 12;
const INODE_EXTREF_KEY: u32 = 13;
/// Objectids of files and directories (`BTRFS_FIRST_FREE_OBJECTID..=LAST_FREE`).
const FIRST_INODE: u64 = BTRFS_SUBVOL_ROOT_INO;
const LAST_INODE: u64 = -256i64 as u64;
/// Bytes of `struct btrfs_inode_item`.
const INODE_ITEM_LEN: usize = 160;
/// Results of one search call: an item can be as large as a leaf (64 KiB nodes at most).
const BUF: usize = 128 * 1024;

/// More inodes than this changed in the subvolume: rescanning everything is as quick.
const MAX_INODES: usize = 1_000_000;
/// More directories than this changed: likewise.
const MAX_DIRS: usize = 200_000;

/// What changed below a root since a generation ([`btrfs_changes_since`]).
#[derive(Debug, Default)]
pub struct BtrfsChanges {
    /// Changed directories relative to the root (the root itself is the empty path).
    pub dirs: Vec<PathBuf>,
    /// Too much changed to say.
    pub full: bool,
}

/// The uuid and generation of the btrfs subvolume holding `path`, when this process may search
/// its tree (else `PermissionDenied`; `Unsupported` off btrfs). Syncs the filesystem first.
pub fn btrfs_position(path: &Path) -> io::Result<([u8; 16], u64)> {
    let fd = open_dir(CWD, path)?;
    if !is_btrfs_fd(&fd)? {
        return Err(io::ErrorKind::Unsupported.into());
    }
    // A position nobody here can replay is useless: try a search first.
    let mut args = SearchArgs::<4096>::boxed();
    let root = (FIRST_INODE, INODE_ITEM_KEY, 0);
    search(&mut args, fd.as_fd(), 0, root, root, 0, |_| false)?;
    // Data written before is allocated now: its writeback changes block counts without
    // changing ctimes, so it must not be left for after the position.
    rustix::fs::syncfs(&fd).map_err(errno)?;
    let a = subvol_args(&fd)?;
    Ok((a.uuid, a.generation))
}

/// The directories at or below `root` that changed after `generation` of the subvolume
/// `uuid`, relative to `root`. An inode counts as changed only if its ctime is no older than
/// `since` (seconds since the Unix epoch, when the position was read; 0 for no such check),
/// so that reading a file or listing a directory, which update atimes, does not. The
/// filesystem is synced first (`syncfs`): inode updates are written to the tree lazily.
/// Fails when `root` is not in that subvolume.
pub fn btrfs_changes_since(
    root: &Path,
    uuid: [u8; 16],
    generation: u64,
    since: u64,
) -> io::Result<BtrfsChanges> {
    let fd = open_dir(CWD, root)?;
    if !is_btrfs_fd(&fd)? {
        return Err(io::ErrorKind::Unsupported.into());
    }
    if subvol_args(&fd)?.uuid != uuid {
        return Err(io::ErrorKind::NotFound.into());
    }
    rustix::fs::syncfs(&fd).map_err(errno)?;
    let full = || {
        Ok(BtrfsChanges {
            dirs: vec![],
            full: true,
        })
    };
    let mut args = SearchArgs::<BUF>::boxed();
    let mut found = Found::default();
    let mut too_many = false;
    search(
        &mut args,
        fd.as_fd(),
        0,
        (FIRST_INODE, 0, 0),
        (LAST_INODE, u8::MAX as u32, u64::MAX),
        generation.saturating_add(1),
        |it| {
            found.add(
                it.objectid,
                it.item_type,
                it.offset,
                &it.data,
                generation,
                since,
            );
            too_many = found.dirs.len() + found.files.len() > MAX_INODES;
            !too_many
        },
    )?;
    if too_many {
        return full();
    }
    let mut dirs = found.dirs;
    for ino in found.files {
        match found.parents.get(&ino) {
            Some(p) => dirs.extend(p),
            // The file's references are in a block that did not change.
            None => dirs.extend(parents(&mut args, fd.as_fd(), ino)?),
        }
        if dirs.len() > MAX_DIRS {
            return full();
        }
    }
    let st = rustix::fs::fstat(&fd).map_err(errno)?;
    let prefix = ino_lookup(fd.as_fd(), st.st_ino)?;
    let mut out = Vec::new();
    for ino in dirs {
        let path = match ino_lookup(fd.as_fd(), ino) {
            Ok(p) => p,
            // Deleted since; its parent changed too.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if let Some(rel) = path.strip_prefix(prefix.as_slice()) {
            let rel = rel.strip_suffix(b"/").unwrap_or(rel);
            out.push(PathBuf::from(OsStr::from_bytes(rel)));
        }
    }
    out.sort();
    Ok(BtrfsChanges {
        dirs: out,
        full: false,
    })
}

/// What a search after a generation found.
#[derive(Default)]
struct Found {
    /// Directories whose inode changed.
    dirs: HashSet<u64>,
    /// Other inodes that changed.
    files: Vec<u64>,
    /// Parent directories by inode, from the reference items seen.
    parents: HashMap<u64, Vec<u64>>,
}

impl Found {
    fn add(&mut self, ino: u64, item_type: u32, offset: u64, d: &[u8], after: u64, since: u64) {
        let le = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().expect("8 bytes"));
        match item_type {
            INODE_ITEM_KEY if d.len() >= INODE_ITEM_LEN => {
                // `struct btrfs_inode_item`: transid at 8, mode at 52, ctime at 124.
                if le(8) <= after || (since > 0 && le(124) < since.saturating_sub(1)) {
                    return;
                }
                let mode = u32::from_le_bytes(d[52..56].try_into().expect("4 bytes"));
                if mode & 0o170000 == 0o040000 {
                    self.dirs.insert(ino);
                } else {
                    self.files.push(ino);
                }
            }
            INODE_REF_KEY => self.parents.entry(ino).or_default().push(offset),
            INODE_EXTREF_KEY => self
                .parents
                .entry(ino)
                .or_default()
                .extend(extref_parents(d)),
            _ => {}
        }
    }
}

/// Parents named by an `INODE_EXTREF` item: packed `struct btrfs_inode_extref` (parent u64,
/// index u64, name length u16, name).
fn extref_parents(d: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(h) = d.get(at..at + 18) {
        out.push(u64::from_le_bytes(h[..8].try_into().expect("8 bytes")));
        at += 18 + u16::from_le_bytes([h[16], h[17]]) as usize;
    }
    out
}

/// The directories holding inode `ino`, from its reference items.
fn parents(args: &mut SearchArgs<BUF>, fd: BorrowedFd<'_>, ino: u64) -> io::Result<Vec<u64>> {
    let mut out = Vec::new();
    let (min, max): (Key, Key) = ((ino, INODE_REF_KEY, 0), (ino, INODE_EXTREF_KEY, u64::MAX));
    search(args, fd, 0, min, max, 0, |it| {
        match it.item_type {
            INODE_REF_KEY => out.push(it.offset),
            INODE_EXTREF_KEY => out.extend(extref_parents(&it.data)),
            _ => {}
        }
        true
    })?;
    Ok(out)
}

/// `struct btrfs_ioctl_ino_lookup_args`.
#[repr(C)]
struct InoLookupArgs {
    treeid: u64,
    objectid: u64,
    name: [u8; 4080],
}
const _: () = assert!(std::mem::size_of::<InoLookupArgs>() == 4096);

const INO_LOOKUP: rustix::ioctl::Opcode =
    opcode::read_write::<InoLookupArgs>(BTRFS_IOCTL_MAGIC, 18);

/// The path of directory `ino` in the subvolume of `fd`, relative to the subvolume's root and
/// ending in `/` (empty for the root). `NotFound` when there is no such directory.
#[allow(unsafe_code)]
fn ino_lookup(fd: BorrowedFd<'_>, ino: u64) -> io::Result<Vec<u8>> {
    let mut args = Box::new(InoLookupArgs {
        treeid: 0,
        objectid: ino,
        name: [0; 4080],
    });
    // SAFETY: INO_LOOKUP is `_IOWR(0x94, 18, struct btrfs_ioctl_ino_lookup_args)`;
    // `InoLookupArgs` has that layout (size checked above) and every bit pattern is valid.
    unsafe {
        ioctl(
            fd,
            rustix::ioctl::Updater::<INO_LOOKUP, InoLookupArgs>::new(&mut args),
        )
    }
    .map_err(errno)?;
    let len = args
        .name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(args.name.len());
    Ok(args.name[..len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrefs_name_every_parent() {
        let mut d = Vec::new();
        for (parent, name) in [(300u64, &b"a"[..]), (301, b"longer name")] {
            d.extend(parent.to_le_bytes());
            d.extend(7u64.to_le_bytes());
            d.extend((name.len() as u16).to_le_bytes());
            d.extend(name);
        }
        assert_eq!(extref_parents(&d), [300, 301]);
        // Cut short: the whole headers are read, nothing past the end.
        assert_eq!(extref_parents(&d[..20]), [300]);
    }

    #[test]
    fn atime_updates_are_not_changes() {
        let mut item = vec![0u8; INODE_ITEM_LEN];
        item[8..16].copy_from_slice(&11u64.to_le_bytes());
        item[52..56].copy_from_slice(&0o040755u32.to_le_bytes());
        item[124..132].copy_from_slice(&1_000u64.to_le_bytes());
        let mut f = Found::default();
        // Changed in transaction 11, ctime 1000: before the position's time, so atime only.
        f.add(300, INODE_ITEM_KEY, 0, &item, 10, 2_000);
        // Not after the position.
        f.add(301, INODE_ITEM_KEY, 0, &item, 11, 0);
        assert!(f.dirs.is_empty());
        f.add(302, INODE_ITEM_KEY, 0, &item, 10, 1_001);
        f.add(303, INODE_ITEM_KEY, 0, &item, 10, 0);
        assert_eq!(f.dirs, HashSet::from([302, 303]));
        item[52..56].copy_from_slice(&0o100644u32.to_le_bytes());
        f.add(304, INODE_ITEM_KEY, 0, &item, 10, 0);
        assert_eq!(f.files, [304]);
    }
}
