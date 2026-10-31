//! dfdu's own snapshot format: a scan saved so it opens at once next time, then rescanned in
//! the background. Lossless where ncdu JSON is not (names are raw bytes, directory states and
//! mount kinds are kept), checked where JSON cannot be (a CRC32C per frame and a footer that
//! counts everything), and written atomically by the caller ([`crate::write_atomic`]).
//!
//! Layout:
//! - `DFDUSNAP`, then the format version as a little-endian `u32`.
//! - Frames: a 12-byte header (compressed length, raw length, CRC32C of the compressed bytes,
//!   each a little-endian `u32`) and the bytes, one zstd frame of at most [`FRAME_RAW`] raw
//!   bytes. Together the raw bytes are one stream of records; a record can span frames.
//! - A header of zeros ends the frames, then the footer: entries and frames (`u64` each), the
//!   CRC32C of the frame CRCs, and the CRC32C of those 20 bytes.
//!
//! The record stream starts with the scan's header (root path, tool version, time, options,
//! the change journal position, when the last full scan started and how long it took) and holds
//! the tree depth first: a directory opens with its fields, its entries follow, and a close
//! record ends it. Numbers are LEB128 varints, signed ones zigzagged. Devices go into
//! a table as they first appear and are referred to by index after.
//!
//! Not kept: classes and protection (the rescan decides them again), btrfs subvolume records
//! and measured extents (likewise), errors' OS codes beyond the kind.

use std::time::Instant;

use dfdu_scan::journal::JournalPosition;
use dfdu_tree::{Ix, NodeFlags, Tree};

mod read;
mod write;

pub use read::{is_snapshot, read, read_header};
pub use write::{Writer, write};

pub const MAGIC: &[u8; 8] = b"DFDUSNAP";
/// The only format read and written. Earlier ones never left pre-release builds; a file in one
/// is refused, and the scan is done again.
pub const VERSION: u32 = 4;
/// Raw bytes per frame.
pub const FRAME_RAW: usize = 1 << 20;
/// Largest frame a reader accepts, raw or compressed.
const FRAME_MAX: usize = 16 << 20;

const DIR: u8 = 1;
const CLOSE: u8 = 2;
const ENTRY: u8 = 3;

/// Flags of a directory that are kept.
const DIR_FLAGS: [NodeFlags; 4] = [
    NodeFlags::MOUNT_ROOT,
    NodeFlags::KERNEL,
    NodeFlags::NETWORK,
    NodeFlags::DUP_MOUNT,
];

/// What the scan was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub tool_version: String,
    /// Seconds since the Unix epoch when the snapshot was written.
    pub written: u64,
    pub cross_mounts: bool,
    pub network: bool,
    /// Where the change journal of the root's volume stood when the scan started, so a resume
    /// can rescan only what changed since.
    pub journal: Option<JournalPosition>,
    /// Seconds since the Unix epoch when the last scan of the whole tree started: the tree may
    /// have been brought up to date from the change journal since, which can miss changes. 0
    /// when not known.
    pub last_full_scan: u64,
    /// How long that full scan took, in milliseconds; 0 when not known.
    pub scan_ms: u64,
}

/// When a tree was last scanned in full, and how long that took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanTiming {
    /// Seconds since the Unix epoch when it started.
    pub last_full_scan: u64,
    /// In milliseconds.
    pub scan_ms: u64,
}

impl ScanTiming {
    /// A full scan that started at `started`, `started_secs` seconds since the Unix epoch, and
    /// ended now.
    #[must_use]
    pub fn ended_now(started: Instant, started_secs: u64) -> ScanTiming {
        ScanTiming {
            last_full_scan: started_secs,
            scan_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

impl Header {
    /// The header of a scan this dfdu writes at `written` (seconds since the Unix epoch): with
    /// its options, where the change journal stood before it started, and its last full scan.
    #[must_use]
    pub fn new(
        written: u64,
        cross_mounts: bool,
        network: bool,
        journal: Option<JournalPosition>,
        full_scan: ScanTiming,
    ) -> Header {
        Header {
            tool_version: env!("CARGO_PKG_VERSION").into(),
            written,
            cross_mounts,
            network,
            journal,
            last_full_scan: full_scan.last_full_scan,
            scan_ms: full_scan.scan_ms,
        }
    }

    /// The last full scan that a resume of this scan keeps: a resume from the change journal
    /// position only adds what changed since, while one without a position rescans the whole
    /// tree, which is a full scan of its own.
    #[must_use]
    pub fn resumed_timing(&self) -> Option<ScanTiming> {
        self.journal.is_some().then(|| ScanTiming {
            last_full_scan: self.full_scan_at(),
            scan_ms: self.scan_ms,
        })
    }

    /// When the last full scan started: [`Header::last_full_scan`], or when the snapshot was
    /// written when that is not known.
    #[must_use]
    pub fn full_scan_at(&self) -> u64 {
        if self.last_full_scan == 0 {
            self.written
        } else {
            self.last_full_scan
        }
    }
}

const OPT_JOURNAL: u64 = 4;

pub struct Loaded {
    pub tree: Tree,
    pub root: Ix,
    pub header: Header,
    pub entries: u64,
}

#[cfg(test)]
mod tests;
