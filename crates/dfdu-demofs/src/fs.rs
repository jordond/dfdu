//! The FUSE side: answers the kernel's requests from the [`Tree`], one session thread, with a
//! thread per delayed directory listing.
//!
//! Inode numbers are node indexes plus one. A listing's offsets are the entries' cookies
//! ([`crate::DirEntry`]), so removing entries while a directory is being read (as `rm -r`
//! does) neither skips nor repeats any.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Context as _;
use fuser::{
    BackgroundSession, BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem,
    FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr,
    ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
    ReplyWrite, Request, SessionACL, TimeOrNow, WriteFlags,
};

use crate::fixture::Fixture;
use crate::tree::{Data, FsError, Kind, Tree};

/// Filesystem subtype: the mount table says `fuse.dfdu-demofs`, which dfdu treats as local.
pub const SUBTYPE: &str = "dfdu-demofs";

const TTL: Duration = Duration::from_secs(1);
const BLOCK: u64 = 4096;

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

impl DemoFs {
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The time now on the fixture's clock.
    fn time(&self) -> i64 {
        self.now + i64::try_from(self.started.elapsed().as_secs()).unwrap_or(0)
    }

    fn attr(&self, t: &Tree, n: usize) -> Result<FileAttr, FsError> {
        let i = t.get(n).ok_or(FsError::NoEnt)?;
        let kind = match i.kind {
            Kind::Dir { .. } => FileType::Directory,
            Kind::File { .. } => FileType::RegularFile,
            Kind::Symlink(_) => FileType::Symlink,
            Kind::Gone => return Err(FsError::NoEnt),
        };
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
    fn stat(&self, t: &Tree, n: usize) -> Result<FileAttr, FsError> {
        let i = t.get(n).ok_or(FsError::NoEnt)?;
        if !i.is_dir()
            && let Some(f) = i.fault
        {
            return Err(f.into());
        }
        self.attr(t, n)
    }

    /// Gives node `n`, just made by `req`, to the user who made it.
    fn made_by(t: &mut Tree, n: usize, req: &Request) {
        (t.nodes[n].uid, t.nodes[n].gid) = (req.uid(), req.gid());
    }

    fn entry(&self, t: &Tree, n: usize, reply: ReplyEntry) {
        match self.stat(t, n) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(e) => reply.error(errno(e)),
        }
    }
}

impl Filesystem for DemoFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let t = self.tree();
        match t.lookup(node(parent), name.as_bytes()) {
            Ok(n) => self.entry(&t, n, reply),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let t = self.tree();
        match self.stat(&t, node(ino)) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let now = self.time();
        let mut t = self.tree();
        let n = node(ino);
        if t.get(n).is_none() {
            return reply.error(Errno::ENOENT);
        }
        if let Some(s) = size
            && let Err(e) = t.truncate(n, s, now)
        {
            return reply.error(errno(e));
        }
        if let Some(m) = mode {
            t.nodes[n].mode = m & 0o7777;
        }
        if let Some(u) = uid {
            t.nodes[n].uid = u;
        }
        if let Some(g) = gid {
            t.nodes[n].gid = g;
        }
        match mtime {
            Some(TimeOrNow::SpecificTime(st)) => {
                let secs = st.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                t.nodes[n].mtime = i64::try_from(secs).unwrap_or(0);
            }
            Some(TimeOrNow::Now) => t.nodes[n].mtime = now,
            None => {}
        }
        match self.attr(&t, n) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match self.tree().get(node(ino)).map(|i| &i.kind) {
            Some(Kind::Symlink(target)) => reply.data(target),
            Some(_) => reply.error(Errno::EINVAL),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn mkdir(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let now = self.time();
        let mut t = self.tree();
        let kind = Kind::Dir {
            parent: 0,
            entries: vec![],
        };
        match t.insert(
            node(parent),
            name.as_bytes(),
            kind,
            now,
            mode & !umask & 0o7777,
        ) {
            Ok(n) => {
                Self::made_by(&mut t, n, req);
                self.entry(&t, n, reply);
            }
            Err(e) => reply.error(errno(e)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let now = self.time();
        match self.tree().unlink(node(parent), name.as_bytes(), now) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let now = self.time();
        match self.tree().rmdir(node(parent), name.as_bytes(), now) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn symlink(
        &self,
        req: &Request,
        parent: INodeNo,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let now = self.time();
        let mut t = self.tree();
        let kind = Kind::Symlink(target.as_os_str().as_bytes().to_vec());
        match t.insert(node(parent), link_name.as_bytes(), kind, now, 0o777) {
            Ok(n) => {
                Self::made_by(&mut t, n, req);
                self.entry(&t, n, reply);
            }
            Err(e) => reply.error(errno(e)),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let now = self.time();
        let r = self.tree().rename(
            node(parent),
            name.as_bytes(),
            node(newparent),
            newname.as_bytes(),
            flags.contains(RenameFlags::RENAME_NOREPLACE),
            flags.contains(RenameFlags::RENAME_EXCHANGE),
            now,
        );
        match r {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn link(
        &self,
        _req: &Request,
        ino: INodeNo,
        newparent: INodeNo,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let mut t = self.tree();
        match t.link(node(ino), node(newparent), newname.as_bytes()) {
            Ok(()) => self.entry(&t, node(ino), reply),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let t = self.tree();
        match self.stat(&t, node(ino)) {
            Ok(_) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.tree().read(node(ino), offset, size) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let now = self.time();
        match self.tree().write(node(ino), offset, data, now) {
            Ok(()) => reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let t = self.tree();
        let Some(i) = t.get(node(ino)) else {
            return reply.error(Errno::ENOENT);
        };
        if !i.is_dir() {
            return reply.error(Errno::ENOTDIR);
        }
        if let Some(f) = i.fault {
            return reply.error(errno(f.into()));
        }
        let delay = i.delay;
        drop(t);
        if delay.is_zero() {
            return reply.opened(FileHandle(0), FopenFlags::empty());
        }
        thread::spawn(move || {
            thread::sleep(delay);
            reply.opened(FileHandle(0), FopenFlags::empty());
        });
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let t = self.tree();
        let n = node(ino);
        let entries = match t.list(n) {
            Ok(e) => e,
            Err(e) => return reply.error(errno(e)),
        };
        let mut listed: Vec<(u64, INodeNo, FileType, &[u8])> = vec![
            (1, ino, FileType::Directory, b"."),
            (2, self::ino(t.parent(n)), FileType::Directory, b".."),
        ];
        for e in entries {
            let kind = match t.get(e.node).map(|i| &i.kind) {
                Some(Kind::Dir { .. }) => FileType::Directory,
                Some(Kind::Symlink(_)) => FileType::Symlink,
                _ => FileType::RegularFile,
            };
            listed.push((e.cookie, self::ino(e.node), kind, &e.name));
        }
        for (cookie, i, kind, name) in listed.into_iter().filter(|e| e.0 > offset) {
            if reply.add(i, cookie, kind, OsStr::from_bytes(name)) {
                break;
            }
        }
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let t = self.tree();
        let free = (self.free0 + self.used0)
            .saturating_sub(t.used())
            .min(self.total);
        let files = 10_000_000;
        reply.statfs(
            self.total / BLOCK,
            free / BLOCK,
            free / BLOCK,
            files,
            files.saturating_sub(t.live() as u64),
            4096,
            255,
            4096,
        );
    }

    fn access(&self, _req: &Request, ino: INodeNo, _mask: fuser::AccessFlags, reply: ReplyEmpty) {
        match self.tree().get(node(ino)) {
            Some(_) => reply.ok(),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn create(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let now = self.time();
        let mut t = self.tree();
        let kind = Kind::File {
            size: 0,
            alloc: None,
            data: Data::Bytes(vec![]),
        };
        let made = t
            .insert(
                node(parent),
                name.as_bytes(),
                kind,
                now,
                mode & !umask & 0o7777,
            )
            .and_then(|n| {
                Self::made_by(&mut t, n, req);
                self.attr(&t, n)
            });
        match made {
            Ok(a) => reply.created(&TTL, &a, Generation(0), FileHandle(0), FopenFlags::empty()),
            Err(e) => reply.error(errno(e)),
        }
    }
}

/// Mounts the tree of `fixture` at the directory `at`, open to every user, and returns once
/// the mount answers. Unmounted when the session is dropped or `umount`ed.
///
/// # Errors
///
/// The fixture does not build, there is no `/dev/fuse`, or the mount fails (it needs root or
/// `fusermount3`).
pub fn mount(fixture: &Fixture, at: &Path) -> anyhow::Result<BackgroundSession> {
    mount_tree(fixture, fixture.build()?, at)
}

/// Mounts `tree` at the directory `at` as [`mount`] does, with the disk and the clock of
/// `fixture` (for a tree built from more than one fixture, [`Fixture::build_at`]).
///
/// # Errors
///
/// The fixture's disk or time is malformed, there is no `/dev/fuse`, or the mount fails.
pub fn mount_tree(fixture: &Fixture, tree: Tree, at: &Path) -> anyhow::Result<BackgroundSession> {
    let total = fixture.disk.total.bytes()?;
    let free0 = fixture.disk.free.bytes()?;
    let used0 = tree.used();
    let fs = DemoFs {
        tree: Mutex::new(tree),
        total,
        free0,
        used0,
        now: fixture.now()?,
        started: Instant::now(),
    };
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
    std::fs::metadata(at).map_err(|e: io::Error| anyhow::anyhow!("{}: {e}", at.display()))?;
    Ok(session)
}
