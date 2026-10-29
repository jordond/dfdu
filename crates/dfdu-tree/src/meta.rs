use std::io;

/// File type of an entry, as reported without following symlinks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
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

/// A btrfs subvolume root.
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
    #[must_use]
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
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
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
                nlink: u32::try_from(m.nlink()).unwrap_or(u32::MAX),
                mount: 0,
            }
        }
        #[cfg(windows)]
        {
            // std has no allocation size, and the volume serial, file id and link count only on
            // nightly: the scanners and the deleter use [`Meta::from_windows`] with what
            // `dfdu-sys` reads instead. The creation time (100 ns units) stands in for the inode
            // number here.
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
    #[must_use]
    pub fn is_hardlink(&self) -> bool {
        self.kind != Kind::Dir && self.nlink > 1
    }

    /// Metadata from what Windows reports ([`WinStat`]). Both Windows backends and the
    /// deleter's check go through this, so their values compare like for like.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "folds a 128-bit file id into the 64 bits the tree stores, on purpose"
    )]
    pub fn from_windows(w: &WinStat) -> Meta {
        // FILETIME counts 100 ns intervals since 1601; std's `modified()` rounds the same way
        // and has no time before 1970. Whole seconds fit an `i64` with room to spare.
        let secs = (w.last_write / 10_000_000).cast_signed() - 11_644_473_600;
        let kind = w.kind();
        Meta {
            kind,
            // A directory's listing reports 0; a handle reports its index's `EndOfFile`. Always
            // 0, so a directory measured as a scan root (or a rescan) is the size it is listed
            // as.
            apparent: if kind == Kind::Dir { 0 } else { w.size },
            alloc: w.alloc,
            mtime: secs.max(0),
            dev: u64::from(w.volume),
            // NTFS file ids are 64-bit file reference numbers in the low half; ReFS uses all
            // 128 bits, folded here into the 64 the tree stores.
            ino: (w.file_id as u64) ^ ((w.file_id >> 64) as u64),
            nlink: w.nlink,
            mount: 0,
        }
    }
}

/// `FILE_ATTRIBUTE_*` bits and reparse tag bits [`WinStat`] looks at.
pub mod win_attr {
    pub const DIRECTORY: u32 = 0x10;
    pub const REPARSE_POINT: u32 = 0x400;
    pub const OFFLINE: u32 = 0x1000;
    pub const RECALL_ON_OPEN: u32 = 0x4_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    /// The reparse tag bit of name surrogates: symlinks, junctions, volume mount points.
    pub const TAG_NAME_SURROGATE: u32 = 0x2000_0000;
}

/// One file as Windows describes it, in a directory enumeration entry or from an open handle.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct WinStat {
    /// `FILE_ATTRIBUTE_*` bits.
    pub attributes: u32,
    /// The reparse tag when `attributes` has `FILE_ATTRIBUTE_REPARSE_POINT`, else 0.
    pub reparse_tag: u32,
    /// `EndOfFile`.
    pub size: u64,
    /// `AllocationSize`: whole clusters for data outside the MFT record, an 8-byte multiple for
    /// data stored in it (small files on NTFS).
    pub alloc: u64,
    /// `LastWriteTime`, 100 ns intervals since 1601.
    pub last_write: u64,
    /// Volume serial number (`GetVolumeInformationByHandleW`, `dwVolumeSerialNumber`).
    pub volume: u32,
    pub file_id: u128,
    pub nlink: u32,
}

impl WinStat {
    /// The kind as std decides it: a reparse point whose tag is a name surrogate (symlink,
    /// junction, volume mount point) is a link and never followed; other reparse points (cloud
    /// placeholders, deduplicated or WOF-compressed files) are what their attributes say.
    #[must_use]
    pub fn kind(&self) -> Kind {
        if self.attributes & win_attr::REPARSE_POINT != 0
            && self.reparse_tag & win_attr::TAG_NAME_SURROGATE != 0
        {
            Kind::Symlink
        } else if self.attributes & win_attr::DIRECTORY != 0 {
            Kind::Dir
        } else {
            Kind::File
        }
    }

    /// Whether opening it can fetch data from elsewhere: cloud placeholders (`OneDrive`
    /// "online-only" files and folders not yet listed) and files moved to offline storage.
    /// Scanners take their sizes from the directory enumeration and never open them.
    #[must_use]
    pub fn recalls_on_open(&self) -> bool {
        self.attributes
            & (win_attr::OFFLINE | win_attr::RECALL_ON_OPEN | win_attr::RECALL_ON_DATA_ACCESS)
            != 0
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
    /// node containing every reference to it.
    pub alloc: u64,
    /// Allocation as `du` counts it, shared data once per file referencing it.
    pub referenced: u64,
    /// Part of `alloc` that is shared with data outside the scan: deleting the node does not
    /// free it.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_metadata() {
        let w = |attributes, reparse_tag| WinStat {
            attributes,
            reparse_tag,
            ..WinStat::default()
        };
        // Symlink, junction, a cloud file, a WOF-compressed file, a directory.
        assert_eq!(w(0x420, 0xA000_000C).kind(), Kind::Symlink);
        assert_eq!(w(0x410, 0xA000_0003).kind(), Kind::Symlink);
        assert_eq!(w(0x420, 0x9000_001A).kind(), Kind::File);
        assert_eq!(w(0x400, 0x8000_0017).kind(), Kind::File);
        assert_eq!(w(0x10, 0).kind(), Kind::Dir);
        assert!(w(0x40_0020, 0x9000_001A).recalls_on_open());
        assert!(!w(0x20, 0).recalls_on_open());
        let m = Meta::from_windows(&WinStat {
            attributes: 0x20,
            size: 100,
            alloc: 104,
            // 2001-09-09 01:46:40 UTC plus 0.9 s.
            last_write: (1_000_000_000 + 11_644_473_600) * 10_000_000 + 9_000_000,
            volume: 0xDEAD_BEEF,
            file_id: (7 << 64) | 5,
            nlink: 2,
            ..WinStat::default()
        });
        assert_eq!(
            (m.kind, m.apparent, m.alloc, m.mtime, m.dev, m.ino, m.nlink),
            (Kind::File, 100, 104, 1_000_000_000, 0xDEAD_BEEF, 2, 2)
        );
        // Before 1970, as std's `modified()` has it.
        let old = Meta::from_windows(&WinStat {
            last_write: 5,
            ..WinStat::default()
        });
        assert_eq!(old.mtime, 0);
        // A directory has no apparent size, whatever its handle says; its index's clusters
        // still count.
        let dir = Meta::from_windows(&WinStat {
            attributes: 0x10,
            size: 4096,
            alloc: 4096,
            ..WinStat::default()
        });
        assert_eq!((dir.kind, dir.apparent, dir.alloc), (Kind::Dir, 0, 4096));
    }
}
