//! Messages from the scanner to the applier (`docs/data-pipeline.md` §4.2).

use dut_tree::{ErrKind, Meta};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u32);

/// Per-job directory id, allocated by the scanner when it creates a task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DirId(pub u32);

impl DirId {
    pub const ROOT: DirId = DirId(0);
}

/// Byte range of a name in a chunk's `names` buffer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NameSpan {
    pub off: u32,
    pub len: u16,
}

#[derive(Clone, Debug)]
pub struct FileRec {
    pub name: NameSpan,
    pub meta: Meta,
}

#[derive(Clone, Debug)]
pub struct SubdirRec {
    pub name: NameSpan,
    pub meta: Meta,
    /// Set when the scanner will not read this directory; no task exists for it, and the
    /// applier closes it at once with this reason.
    pub skip: Option<SkipReason>,
}

#[derive(Clone, Debug)]
pub struct EntryError {
    pub name: NameSpan,
    pub err: ErrKind,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SkipReason {
    OtherFilesystem,
    Excluded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkStatus {
    Ok,
    Panicked(String),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListingStatus {
    Complete,
    Partial {
        panicked: u32,
        cancelled: u32,
        read_error: Option<ErrKind>,
    },
    Failed(ErrKind),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub job: JobId,
    pub dir: DirId,
    pub seq: u32,
    /// Stat of the job root itself, sent in the root's first chunk.
    pub root_meta: Option<Meta>,
    pub names: Vec<u8>,
    pub files: Vec<FileRec>,
    /// Subdirectories, with ids `first_subdir, first_subdir + 1, ...` in order.
    pub first_subdir: DirId,
    pub subdirs: Vec<SubdirRec>,
    pub errors: Vec<EntryError>,
    pub status: ChunkStatus,
}

impl Chunk {
    pub fn name(&self, s: NameSpan) -> &[u8] {
        &self.names[s.off as usize..s.off as usize + s.len as usize]
    }

    /// Bytes this message holds on the heap plus its fixed size; what the byte budget counts.
    pub fn size(&self) -> usize {
        use std::mem::size_of;
        size_of::<ScanMsg>()
            + self.names.capacity()
            + self.files.capacity() * size_of::<FileRec>()
            + self.subdirs.capacity() * size_of::<SubdirRec>()
            + self.errors.capacity() * size_of::<EntryError>()
    }
}

#[derive(Clone, Debug)]
pub enum ScanMsg {
    Chunk(Chunk),
    /// Exactly one per directory that has a task.
    Close {
        job: JobId,
        dir: DirId,
        chunks: u32,
        status: ListingStatus,
    },
    /// Goes through the stalled worker's outbox, in order.
    Stalled {
        job: JobId,
        dir: DirId,
    },
    /// Every non-abandoned task has had its `Close` flushed (§5.7).
    Quiescent {
        submitted_seq: u64,
    },
}

impl ScanMsg {
    pub fn size(&self) -> usize {
        match self {
            ScanMsg::Chunk(c) => c.size(),
            _ => std::mem::size_of::<ScanMsg>(),
        }
    }
}

/// What one flush delivers. `bytes` was acquired from the [`crate::Budget`]; the applier releases
/// it after applying the batch.
#[derive(Debug)]
pub struct Batch {
    pub msgs: Vec<ScanMsg>,
    pub bytes: usize,
}
