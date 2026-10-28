//! Messages from the scanner to the applier.

use dfdu_tree::{Class, ErrKind, Meta, Subvol};

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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    /// The directory is a btrfs subvolume root.
    pub subvol: Option<Subvol>,
    /// Files on this filesystem can share extents (btrfs, XFS, bcachefs), so measuring them is
    /// worthwhile.
    pub shares_extents: bool,
    /// btrfs data profile name and raw bytes stored per byte of data (2 for DUP or RAID1).
    pub profile: Option<(&'static str, f64)>,
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
    /// The directory's mount differs from its parent's. btrfs subvolumes are not mount roots
    /// unless mounted.
    pub mount_root: bool,
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
    /// A network filesystem; scanned only with `JobOptions::network`.
    Network,
    /// A kernel filesystem (`/proc`, `/sys`, cgroups); never scanned.
    Kernel,
    /// With `cross_mounts`: a mount of something this job already scans elsewhere (a bind
    /// mount, a second mount of a subvolume, or a mount loop).
    DuplicateMount,
    Excluded,
}

/// An entry of a chunk, by its index in `files` or `subdirs`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    File(u32),
    Subdir(u32),
}

/// What classification found for an entry (`crate::classify`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mark {
    Class(Class),
    /// The top of a protected subtree.
    Protected,
    /// The top of a subtree an exception lifts out of the protected one around it.
    Excepted,
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
    /// What the backend found about the directory's filesystem, for job roots and directories
    /// whose `dev` differs from their parent's; sent in its first chunk.
    pub probe: Option<Box<Probe>>,
    pub names: Vec<u8>,
    pub files: Vec<FileRec>,
    /// Subdirectories, with ids `first_subdir, first_subdir + 1, ...` in order.
    pub first_subdir: DirId,
    pub subdirs: Vec<SubdirRec>,
    pub errors: Vec<EntryError>,
    /// Classes and protection of entries, for the few that have one.
    pub marks: Vec<(Entry, Mark)>,
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
            + self.marks.capacity() * size_of::<(Entry, Mark)>()
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
        /// Classes decided once a listing was complete: the directory's own, which needed a
        /// look inside it, and its subdirectories' that needed its whole listing (a wide
        /// directory). Applied with the `Close`, when every chunk is in.
        classes: Vec<(DirId, Class)>,
    },
    /// Goes through the stalled worker's outbox, in order.
    Stalled {
        job: JobId,
        dir: DirId,
    },
    /// Every non-abandoned task has had its `Close` flushed.
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
