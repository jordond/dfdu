//! The requests the kernel forwards, each answered from the tree under its lock.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::thread;
use std::time::UNIX_EPOCH;

use fuser::{
    BsdFileFlags, Errno, FileHandle, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};

use super::{BLOCK, DemoFs, FILES, TTL, attr, entry, errno, listing, made_by, node, stat};
use crate::tree::Kind;

impl Filesystem for DemoFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let t = self.tree();
        match t.lookup(node(parent), name.as_bytes()) {
            Ok(n) => entry(&t, n, reply),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let t = self.tree();
        match stat(&t, node(ino)) {
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
        match attr(&t, n) {
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
        match t.insert(
            node(parent),
            name.as_bytes(),
            Kind::dir(),
            now,
            mode & !umask & 0o7777,
        ) {
            Ok(n) => {
                made_by(&mut t, n, req);
                entry(&t, n, reply);
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
                made_by(&mut t, n, req);
                entry(&t, n, reply);
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
            Ok(()) => entry(&t, node(ino), reply),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let t = self.tree();
        match stat(&t, node(ino)) {
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
        let listed = match listing(&t, node(ino), offset) {
            Ok(l) => l,
            Err(e) => return reply.error(errno(e)),
        };
        for l in listed {
            if reply.add(l.ino, l.cookie, l.kind, OsStr::from_bytes(l.name)) {
                break;
            }
        }
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let t = self.tree();
        let free = self.free(&t);
        reply.statfs(
            self.total / BLOCK,
            free / BLOCK,
            free / BLOCK,
            FILES,
            FILES.saturating_sub(t.live() as u64),
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
        let created = t
            .insert(
                node(parent),
                name.as_bytes(),
                Kind::stored(vec![]),
                now,
                mode & !umask & 0o7777,
            )
            .and_then(|n| {
                made_by(&mut t, n, req);
                attr(&t, n)
            });
        match created {
            Ok(a) => reply.created(&TTL, &a, Generation(0), FileHandle(0), FopenFlags::empty()),
            Err(e) => reply.error(errno(e)),
        }
    }
}
