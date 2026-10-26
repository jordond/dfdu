use std::io;

/// File type of an entry, as reported without following symlinks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Metadata of one entry, as produced by a scanner backend.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub kind: Kind,
    /// File length in bytes.
    pub apparent: u64,
    /// Bytes allocated on disk (`st_blocks * 512` or the platform equivalent).
    pub alloc: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    /// Mount id of a directory (Linux `statx` `STATX_MNT_ID`); 0 when not known, in which case
    /// `dev` decides mount boundaries.
    pub mount: u64,
}

/// A btrfs subvolume root (`docs/btrfs.md` §3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subvol {
    pub id: u64,
    pub uuid: [u8; 16],
    /// For snapshots, the uuid of the subvolume the snapshot was taken of.
    pub parent_uuid: Option<[u8; 16]>,
    pub read_only: bool,
    /// Creation time, seconds since the Unix epoch.
    pub otime: i64,
    /// Quota group `(referenced, exclusive)` bytes, when quotas are on and the scan runs as
    /// root. btrfs's own numbers; they can lag until a quota rescan.
    pub qgroup: Option<(u64, u64)>,
}

impl Meta {
    /// Metadata as std reports it, without following symlinks when `m` came from
    /// `symlink_metadata` or a directory entry.
    pub fn from_metadata(m: &std::fs::Metadata) -> Meta {
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
                mount: 0,
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
                mount: 0,
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
                mount: 0,
            }
        }
    }

    /// A file with `nlink > 1` takes part in hard-link charging.
    pub fn is_hardlink(&self) -> bool {
        self.kind != Kind::Dir && self.nlink > 1
    }
}

/// Error classes recorded on entries and directories.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ErrKind {
    PermissionDenied,
    NotFound,
    /// Raw OS error code.
    Os(i32),
    /// Abandoned after a filesystem call did not return.
    Stalled,
    Other,
}

impl From<&io::Error> for ErrKind {
    fn from(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::PermissionDenied => ErrKind::PermissionDenied,
            io::ErrorKind::NotFound => ErrKind::NotFound,
            _ => match e.raw_os_error() {
                Some(code) => ErrKind::Os(code),
                None => ErrKind::Other,
            },
        }
    }
}

impl std::fmt::Display for ErrKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrKind::PermissionDenied => f.write_str("permission denied"),
            ErrKind::NotFound => f.write_str("removed during the scan"),
            ErrKind::Stalled => f.write_str("not responding"),
            ErrKind::Other => f.write_str("could not be read"),
            ErrKind::Os(code) => {
                // The system's message ("Input/output error (os error 5)") without the code.
                let msg = io::Error::from_raw_os_error(*code).to_string();
                let msg = msg.split(" (os error").next().unwrap_or(&msg);
                let mut c = msg.chars();
                match c.next() {
                    Some(first) => write!(f, "{}{}", first.to_lowercase(), c.as_str()),
                    None => write!(f, "error {code}"),
                }
            }
        }
    }
}

/// Aggregated sizes and counts of a node and everything below it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Totals {
    pub apparent: u64,
    /// Disk usage: allocation, with data shared between files counted once, at the lowest
    /// node containing every reference to it (`docs/btrfs.md` §3.4).
    pub alloc: u64,
    /// Allocation as `du` counts it, shared data once per file referencing it.
    pub referenced: u64,
    /// Part of `alloc` that is shared with data outside the scan: deleting the node does not
    /// free it (`docs/btrfs.md` B5).
    pub shared_out: u64,
    /// Number of nodes, the node itself included.
    pub items: u64,
    /// Number of nodes with an error.
    pub errors: u64,
    /// Directories that are mount roots (deletion never crosses them).
    pub mounts: u64,
    /// Read-only btrfs subvolume roots (their contents cannot be deleted).
    pub ro_subvols: u64,
}

impl Totals {
    pub fn add(&mut self, o: &Totals) {
        self.apparent += o.apparent;
        self.alloc += o.alloc;
        self.referenced += o.referenced;
        self.shared_out += o.shared_out;
        self.items += o.items;
        self.errors += o.errors;
        self.mounts += o.mounts;
        self.ro_subvols += o.ro_subvols;
    }

    /// Subtracts `o`. Underflow is a bookkeeping bug: it panics in debug builds and
    /// saturates in release builds, where the invariant check reports it.
    pub fn sub(&mut self, o: &Totals) {
        debug_assert!(
            self.apparent >= o.apparent
                && self.alloc >= o.alloc
                && self.referenced >= o.referenced
                && self.shared_out >= o.shared_out
                && self.items >= o.items
                && self.errors >= o.errors
                && self.mounts >= o.mounts
                && self.ro_subvols >= o.ro_subvols,
            "totals underflow: {self:?} - {o:?}"
        );
        self.apparent = self.apparent.saturating_sub(o.apparent);
        self.alloc = self.alloc.saturating_sub(o.alloc);
        self.referenced = self.referenced.saturating_sub(o.referenced);
        self.shared_out = self.shared_out.saturating_sub(o.shared_out);
        self.items = self.items.saturating_sub(o.items);
        self.errors = self.errors.saturating_sub(o.errors);
        self.mounts = self.mounts.saturating_sub(o.mounts);
        self.ro_subvols = self.ro_subvols.saturating_sub(o.ro_subvols);
    }
}
