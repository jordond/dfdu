//! The FUSE side: answers the kernel's requests from the [`Tree`], one session thread, with a
//! thread per delayed directory listing.
//!
//! Inode numbers are node indexes plus one. A listing's offsets are the entries' cookies
//! ([`crate::DirEntry`]), so removing entries while a directory is being read (as `rm -r`
//! does) neither skips nor repeats any.

mod ops;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Context as _;
use fuser::{
    BackgroundSession, Config, Errno, FileAttr, FileType, Generation, INodeNo, MountOption,
    ReplyEntry, Request, SessionACL,
};

use crate::Error;
use crate::fixture::Fixture;
use crate::tree::{FsError, Kind, Tree};

/// Filesystem subtype: the mount table says `fuse.dfdu-demofs`, which dfdu treats as local.
pub const SUBTYPE: &str = "dfdu-demofs";

const TTL: Duration = Duration::from_secs(1);
const BLOCK: u64 = 4096;
/// Inodes `statfs` reports the filesystem has room for.
const FILES: u64 = 10_000_000;

struct DemoFs {
    tree: Mutex<Tree>,
    /// Size of the filesystem, and its free space while the tree uses `used0`.
    total: u64,
    free0: u64,
    used0: u64,
    /// The fixture's `now` and when the mount was made: times set by changes run on from it.
    now: i64,
    started: Instant,
}

impl DemoFs {
    /// Serves `tree` with the disk and the clock of `fixture`, the clock starting now.
    fn new(fixture: &Fixture, tree: Tree) -> anyhow::Result<DemoFs> {
        Ok(DemoFs {
            total: fixture.disk.total.bytes().context("disk total")?,
            free0: fixture.disk.free.bytes().context("disk free")?,
            used0: tree.used(),
            tree: Mutex::new(tree),
            now: fixture.now()?,
            started: Instant::now(),
        })
    }

    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The time now on the fixture's clock.
    fn time(&self) -> i64 {
        self.now + i64::try_from(self.started.elapsed().as_secs()).unwrap_or(0)
    }

    /// Free space with `t` on the disk: what the fixture says, plus what has been freed since
    /// (less what has been written), never more than the disk.
    fn free(&self, t: &Tree) -> u64 {
        (self.free0 + self.used0)
            .saturating_sub(t.used())
            .min(self.total)
    }
}

fn errno(e: FsError) -> Errno {
    match e {
        FsError::NoEnt => Errno::ENOENT,
        FsError::NotDir => Errno::ENOTDIR,
        FsError::IsDir => Errno::EISDIR,
        FsError::NotEmpty => Errno::ENOTEMPTY,
        FsError::Exist => Errno::EEXIST,
        FsError::Inval => Errno::EINVAL,
        FsError::Denied => Errno::EACCES,
        FsError::Io => Errno::EIO,
        FsError::TooBig => Errno::EFBIG,
    }
}

fn node(ino: INodeNo) -> usize {
    usize::try_from(u64::from(ino).saturating_sub(1)).unwrap_or(usize::MAX)
}

fn ino(n: usize) -> INodeNo {
    INodeNo(n as u64 + 1)
}

/// The type a listing and `stat` give a node; none for a removed one.
fn file_type(kind: &Kind) -> Option<FileType> {
    match kind {
        Kind::Dir { .. } => Some(FileType::Directory),
        Kind::File { .. } => Some(FileType::RegularFile),
        Kind::Symlink(_) => Some(FileType::Symlink),
        Kind::Gone => None,
    }
}

fn attr(t: &Tree, n: usize) -> Result<FileAttr, FsError> {
    let i = t.get(n).ok_or(FsError::NoEnt)?;
    let kind = file_type(&i.kind).ok_or(FsError::NoEnt)?;
    let time = UNIX_EPOCH + Duration::from_secs(u64::try_from(i.mtime).unwrap_or(0));
    let nlink = if i.is_dir() {
        2 + u32::try_from(t.subdirs(n)).unwrap_or(u32::MAX - 2)
    } else {
        i.nlink
    };
    Ok(FileAttr {
        ino: ino(n),
        size: t.size(n),
        blocks: t.alloc(n).div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind,
        perm: u16::try_from(i.mode & 0o7777).unwrap_or(0o644),
        nlink,
        uid: i.uid,
        gid: i.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    })
}

/// The attributes of `n`, failing with its fault if it is a faulty file.
fn stat(t: &Tree, n: usize) -> Result<FileAttr, FsError> {
    let i = t.get(n).ok_or(FsError::NoEnt)?;
    if !i.is_dir()
        && let Some(f) = i.fault
    {
        return Err(f.into());
    }
    attr(t, n)
}

/// Gives node `n`, just made by `req`, to the user who made it.
fn made_by(t: &mut Tree, n: usize, req: &Request) {
    (t.nodes[n].uid, t.nodes[n].gid) = (req.uid(), req.gid());
}

fn entry(t: &Tree, n: usize, reply: ReplyEntry) {
    match stat(t, n) {
        Ok(a) => reply.entry(&TTL, &a, Generation(0)),
        Err(e) => reply.error(errno(e)),
    }
}

/// One name in a directory listing.
#[derive(Debug, PartialEq, Eq)]
struct Listed<'a> {
    cookie: u64,
    ino: INodeNo,
    kind: FileType,
    name: &'a [u8],
}

/// The listing of directory `n` after the cookie `offset`: `.` (1), `..` (2), then its entries.
fn listing(t: &Tree, n: usize, offset: u64) -> Result<Vec<Listed<'_>>, FsError> {
    let dots = [(1, n, &b"."[..]), (2, t.parent(n), b"..")].map(|(cookie, d, name)| Listed {
        cookie,
        ino: ino(d),
        kind: FileType::Directory,
        name,
    });
    let entries = t.list(n)?.iter().map(|e| Listed {
        cookie: e.cookie,
        ino: ino(e.node),
        kind: t
            .get(e.node)
            .and_then(|i| file_type(&i.kind))
            .unwrap_or(FileType::RegularFile),
        name: &e.name,
    });
    Ok(dots
        .into_iter()
        .chain(entries)
        .filter(|l| l.cookie > offset)
        .collect())
}

/// Mounts the tree of `fixture` at the directory `at`, open to every user, and returns once
/// the mount answers. Unmounted when the session is dropped or `umount`ed.
///
/// # Errors
///
/// The fixture does not build, there is no `/dev/fuse`, or the mount fails (it needs root or
/// `fusermount3`).
pub fn mount(fixture: &Fixture, at: &Path) -> Result<BackgroundSession, Error> {
    mount_tree(fixture, fixture.build()?, at)
}

/// Mounts `tree` at the directory `at` as [`mount`] does, with the disk and the clock of
/// `fixture` (for a tree built from more than one fixture, [`Fixture::build_at`]).
///
/// # Errors
///
/// The fixture's disk or time is malformed, there is no `/dev/fuse`, or the mount fails.
pub fn mount_tree(fixture: &Fixture, tree: Tree, at: &Path) -> Result<BackgroundSession, Error> {
    Ok(spawn(fixture, tree, at)?)
}

fn spawn(fixture: &Fixture, tree: Tree, at: &Path) -> anyhow::Result<BackgroundSession> {
    let fs = DemoFs::new(fixture, tree)?;
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(fixture.disk.device.clone()),
        // A kernel option: fuser passes `Subtype` only to fusermount.
        MountOption::CUSTOM(format!("subtype={SUBTYPE}")),
    ];
    config.acl = SessionACL::All;
    let session = fuser::spawn_mount(fs, at, &config)
        .with_context(|| format!("mounting at {}", at.display()))?;
    // The mount is usable once the root answers.
    std::fs::metadata(at).with_context(|| at.display().to_string())?;
    Ok(session)
}

#[cfg(test)]
mod tests;
