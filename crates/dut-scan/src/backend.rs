//! Filesystem access. A backend opens directories and streams their entries in blocks; the
//! portable backend uses `std::fs` and is the reference the fast per-OS backends are tested
//! against.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dut_tree::{ErrKind, Kind, Meta};

use crate::proto::NameSpan;

/// One entry read from a directory.
#[derive(Debug)]
pub struct BlockEntry<L> {
    pub name: NameSpan,
    pub meta: Result<Meta, ErrKind>,
    /// Location of the entry, for directories, so a task can be created for it.
    pub child: Option<L>,
}

/// A block of entries with their names in one buffer.
#[derive(Debug)]
pub struct Block<L> {
    pub names: Vec<u8>,
    pub entries: Vec<BlockEntry<L>>,
}

impl<L> Default for Block<L> {
    fn default() -> Self {
        Block {
            names: Vec::new(),
            entries: Vec::new(),
        }
    }
}

impl<L> Block<L> {
    pub fn push(&mut self, name: &[u8], meta: Result<Meta, ErrKind>, child: Option<L>) {
        let len = name.len().min(u16::MAX as usize);
        let off = self.names.len() as u32;
        self.names.extend_from_slice(&name[..len]);
        self.entries.push(BlockEntry {
            name: NameSpan {
                off,
                len: len as u16,
            },
            meta,
            child,
        });
    }

    pub fn name(&self, s: NameSpan) -> &[u8] {
        &self.names[s.off as usize..s.off as usize + s.len as usize]
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.entries.clear();
    }
}

pub trait DirStream: Send {
    type Loc;

    /// Appends entries to `out` until `max_entries` entries or `max_name_bytes` of names are in
    /// it, or the directory ends. Returns `Ok(true)` if more entries may follow and `Ok(false)`
    /// at the end. An error after a partial read is returned on the next call, so the entries
    /// already read are kept. `ErrorKind::WouldBlock` means "not ready, call again later"; only
    /// simulated backends return it.
    fn next_block(
        &mut self,
        out: &mut Block<Self::Loc>,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<bool>;
}

pub trait Backend: Send + Sync + 'static {
    type Loc: Send + Clone + std::fmt::Debug + 'static;
    type Stream: DirStream<Loc = Self::Loc>;

    fn loc_from_path(&self, path: &Path) -> Self::Loc;
    /// Stat of a job root, following a symlink given as the root.
    fn stat_root(&self, loc: &Self::Loc) -> io::Result<Meta>;
    fn open(&self, loc: &Self::Loc) -> io::Result<Self::Stream>;
}

/// `std::fs` backend: one `stat` per entry, no OS-specific fast paths.
#[derive(Default, Clone, Copy, Debug)]
pub struct PortableBackend;

pub struct PortableStream {
    iter: fs::ReadDir,
    pending_err: Option<io::Error>,
    done: bool,
}

pub fn meta_from_std(m: &fs::Metadata) -> Meta {
    let ft = m.file_type();
    let kind = if ft.is_dir() {
        Kind::Dir
    } else if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Meta {
            kind,
            apparent: m.size(),
            alloc: m.blocks() * 512,
            mtime,
            dev: m.dev(),
            ino: m.ino(),
            nlink: m.nlink() as u32,
        }
    }
    #[cfg(windows)]
    {
        // Allocation size, volume id, file id and link count need the Windows backend (M2);
        // std exposes the file id only on nightly. The creation time (100 ns units) stands in
        // for the inode number, so a file or directory replaced since the scan is told apart.
        use std::os::windows::fs::MetadataExt;
        Meta {
            kind,
            apparent: m.len(),
            alloc: m.len(),
            mtime,
            dev: 0,
            ino: m.creation_time(),
            nlink: 1,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Meta {
            kind,
            apparent: m.len(),
            alloc: m.len(),
            mtime,
            dev: 0,
            ino: 0,
            nlink: 1,
        }
    }
}

impl DirStream for PortableStream {
    type Loc = PathBuf;

    fn next_block(
        &mut self,
        out: &mut Block<PathBuf>,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<bool> {
        if let Some(e) = self.pending_err.take() {
            self.done = true;
            return Err(e);
        }
        if self.done {
            return Ok(false);
        }
        let start = out.entries.len();
        while out.entries.len() - start < max_entries && out.names.len() < max_name_bytes {
            match self.iter.next() {
                None => {
                    self.done = true;
                    return Ok(false);
                }
                Some(Err(e)) => {
                    if out.entries.len() == start {
                        self.done = true;
                        return Err(e);
                    }
                    self.pending_err = Some(e);
                    return Ok(true);
                }
                Some(Ok(entry)) => {
                    let name = entry.file_name();
                    let meta = entry.metadata().map(|m| meta_from_std(&m));
                    let child = match &meta {
                        Ok(m) if m.kind == Kind::Dir => Some(entry.path()),
                        _ => None,
                    };
                    out.push(
                        name.as_encoded_bytes(),
                        meta.map_err(|e| ErrKind::from(&e)),
                        child,
                    );
                }
            }
        }
        Ok(true)
    }
}

impl Backend for PortableBackend {
    type Loc = PathBuf;
    type Stream = PortableStream;

    fn loc_from_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    fn stat_root(&self, loc: &PathBuf) -> io::Result<Meta> {
        fs::metadata(loc).map(|m| meta_from_std(&m))
    }

    fn open(&self, loc: &PathBuf) -> io::Result<PortableStream> {
        Ok(PortableStream {
            iter: fs::read_dir(loc)?,
            pending_err: None,
            done: false,
        })
    }
}
