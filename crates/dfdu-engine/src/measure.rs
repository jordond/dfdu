//! Extent measurement: the applier asks for files on filesystems that
//! can share extents to be measured; measurer threads open each file, check it is still the
//! scanned inode, read its extent map and reply. Like the deleter, they never touch the tree.

use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{Sender, unbounded};
use dfdu_tree::{ErrKind, NodeRef};

use crate::extents::{Inode, Measurement};

/// Files of one directory to measure. The measurer opens the directory once and each file
/// relative to it.
#[derive(Clone, Debug)]
pub struct MeasureReq {
    /// Requests of an abandoned measurement round are answered but ignored.
    pub round: u64,
    pub dir: PathBuf,
    pub files: Vec<MeasureFile>,
}

#[derive(Clone, Debug)]
pub struct MeasureFile {
    pub node: NodeRef,
    pub name: Vec<u8>,
    pub inode: Inode,
}

impl MeasureReq {
    pub fn path(&self, f: &MeasureFile) -> PathBuf {
        self.dir.join(&*dfdu_tree::name_os(&f.name))
    }
}

#[derive(Clone, Debug)]
pub enum MeasureResult {
    Measured(Measurement),
    /// The path is now another file, or not a regular file.
    Changed,
    Failed(ErrKind),
}

#[derive(Clone, Debug)]
pub struct MeasureMsg {
    pub round: u64,
    pub node: NodeRef,
    pub inode: Inode,
    pub result: MeasureResult,
}

/// Starts `n` measurer threads; results go to `out`, one batch per request. They exit when the
/// returned sender is dropped (a thread stuck in a filesystem call is left behind, like a stuck
/// scan worker).
pub fn spawn_measurers(n: usize, out: Sender<Vec<MeasureMsg>>) -> Sender<MeasureReq> {
    let (tx, rx) = unbounded();
    for i in 0..n.max(1) {
        let (rx, out) = (rx.clone(), out.clone());
        thread::Builder::new()
            .name(format!("dfdu-measure-{i}"))
            .spawn(move || {
                let mut m = Measurer::default();
                #[cfg(target_os = "linux")]
                {
                    m.privileged = dfdu_sys::is_privileged();
                }
                for r in rx {
                    let _ = out.send(m.batch(&r));
                }
            })
            .expect("spawn measurer");
    }
    tx
}

/// One measurer thread's state: reused extent buffers and filesystem identities by device.
#[derive(Default)]
struct Measurer {
    #[cfg(target_os = "linux")]
    reader: dfdu_sys::ExtentReader,
    #[cfg(target_os = "linux")]
    fsids: std::collections::HashMap<u64, [u8; 16]>,
    /// Root: try the tree search for exact sizes.
    #[cfg(target_os = "linux")]
    privileged: bool,
}

impl Measurer {
    fn batch(&mut self, r: &MeasureReq) -> Vec<MeasureMsg> {
        let results = self.measure_dir(r);
        r.files
            .iter()
            .zip(results)
            .map(|(f, result)| MeasureMsg {
                round: r.round,
                node: f.node,
                inode: f.inode,
                result,
            })
            .collect()
    }
}

/// Turns a FIEMAP extent list into a measurement: unshared bytes and shared physical ranges,
/// as `btrfs filesystem du` counts them. Inline data lives in metadata and counts as 0;
/// delayed allocations count as unshared.
///
/// A compressed extent's FIEMAP length is its uncompressed length, so its range
/// `[physical, physical + length)` reaches past its data on disk into the extents stored after
/// it. Shared ranges are counted once where they overlap, so a run of shared compressed
/// extents counts about the space it spans on disk: still an upper bound of its size, and far
/// closer than the sum of uncompressed lengths.
#[cfg(target_os = "linux")]
pub fn from_extents(fs: [u8; 16], extents: &[dfdu_sys::Extent]) -> Measurement {
    use dfdu_sys::Extent;
    let mut m = Measurement {
        fs,
        ..Default::default()
    };
    for e in extents {
        let f = e.flags;
        if f & Extent::DATA_INLINE != 0 {
            continue;
        }
        if f & (Extent::UNKNOWN | Extent::DELALLOC) != 0 {
            m.excl += e.length;
            continue;
        }
        m.compressed |= f & Extent::ENCODED != 0;
        if f & Extent::SHARED != 0 {
            m.ranges.push((e.physical, e.physical + e.length));
        } else {
            m.excl += e.length;
        }
    }
    m
}

/// Exact measurement (as root): every extent at its size on disk
/// (compressed size for compressed data, the whole extent where only part is still
/// referenced), once per file. Extents FIEMAP marks shared for the part of the file they cover
/// are keyed by their disk address, so every file referencing one counts the same extent once.
/// Inline data counts at its stored size, as `compsize` does.
#[cfg(target_os = "linux")]
pub fn from_items(
    fs: [u8; 16],
    items: &[dfdu_sys::FileExtent],
    fiemap: &[dfdu_sys::Extent],
) -> Measurement {
    use dfdu_sys::{Extent, FileExtentKind};
    let mut m = Measurement {
        fs,
        ..Default::default()
    };
    let mut extents: std::collections::BTreeMap<u64, (u64, bool)> = Default::default();
    for it in items {
        if it.kind == FileExtentKind::Inline {
            m.excl += it.disk_num_bytes;
            continue;
        }
        if it.disk_bytenr == 0 {
            // A hole.
            continue;
        }
        let end = it.file_offset + it.num_bytes;
        let shared = fiemap.iter().any(|e| {
            e.flags & Extent::SHARED != 0
                && e.logical < end
                && it.file_offset < e.logical + e.length
        });
        let e = extents
            .entry(it.disk_bytenr)
            .or_insert((it.disk_num_bytes, false));
        e.1 |= shared;
    }
    for (at, (len, shared)) in extents {
        if shared {
            m.points.push((at, len));
        } else {
            m.excl += len;
        }
    }
    m
}

#[cfg(target_os = "linux")]
impl Measurer {
    fn measure_dir(&mut self, r: &MeasureReq) -> Vec<MeasureResult> {
        use rustix::fs::{CWD, Mode, OFlags};
        let dir = rustix::fs::openat(
            CWD,
            &r.dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        );
        match dir {
            Ok(d) => r.files.iter().map(|f| self.measure_at(&d, f)).collect(),
            Err(e) => r.files.iter().map(|_| from_errno(e)).collect(),
        }
    }

    fn measure_at(&mut self, dir: &std::os::fd::OwnedFd, f: &MeasureFile) -> MeasureResult {
        use rustix::fs::{Mode, OFlags};
        let name = &*dfdu_tree::name_os(&f.name);
        let base = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        // O_NOATIME keeps reads from changing atime, but only the owner (or root) may use it.
        let fd = match rustix::fs::openat(dir, name, base | OFlags::NOATIME, Mode::empty()) {
            Err(rustix::io::Errno::PERM) => rustix::fs::openat(dir, name, base, Mode::empty()),
            r => r,
        };
        let fd = match fd {
            Ok(fd) => fd,
            Err(e) => return from_errno(e),
        };
        let st = match rustix::fs::fstat(&fd) {
            Ok(st) => st,
            Err(e) => return from_errno(e),
        };
        let regular =
            rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::RegularFile;
        if !regular || (st.st_dev as u64, st.st_ino as u64) != f.inode {
            return MeasureResult::Changed;
        }
        // btrfs: one identity for all subvolumes of the filesystem; elsewhere the device.
        let fs = *self.fsids.entry(f.inode.0).or_insert_with(|| {
            dfdu_sys::btrfs_fsid(&fd).unwrap_or_else(|_| {
                let mut id = [0u8; 16];
                id[..8].copy_from_slice(&f.inode.0.to_le_bytes());
                id
            })
        });
        let ext = match self.reader.fiemap(&fd) {
            Ok(ext) => ext,
            Err(e) => return MeasureResult::Failed(ErrKind::from(&e)),
        };
        // As root on btrfs the extent tree gives exact sizes; without the privilege (or on
        // another filesystem) the tree search fails and FIEMAP alone is used.
        if self.privileged
            && let Ok(items) = self.reader.file_extents(&fd, f.inode.1)
        {
            let mut m = from_items(fs, &items, &ext);
            m.exact = true;
            return MeasureResult::Measured(m);
        }
        MeasureResult::Measured(from_extents(fs, &ext))
    }
}

#[cfg(target_os = "linux")]
fn from_errno(e: rustix::io::Errno) -> MeasureResult {
    match e {
        // Gone, or replaced by a symlink: the scan's entry no longer exists.
        rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
            MeasureResult::Changed
        }
        e => MeasureResult::Failed(ErrKind::from(&std::io::Error::from_raw_os_error(
            e.raw_os_error(),
        ))),
    }
}

#[cfg(not(target_os = "linux"))]
impl Measurer {
    fn measure_dir(&mut self, r: &MeasureReq) -> Vec<MeasureResult> {
        r.files
            .iter()
            .map(|_| MeasureResult::Failed(ErrKind::Other))
            .collect()
    }
}
