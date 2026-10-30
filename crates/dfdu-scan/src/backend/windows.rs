//! Windows backend: `GetFileInformationByHandleEx` with `FileIdExtdDirectoryInfo` into a 64 KiB
//! buffer per directory, which returns each entry's sizes, attributes, reparse tag, write time
//! and file id with its name, so no entry is opened to learn its size.
//!
//! Reading a block ([`DirStream::next_raw`]) takes the entries from that buffer, metadata
//! included. Stat'ing it ([`RawBlock::stat`]) converts the names and asks for the one thing the
//! enumeration lacks, the link count, for regular files on volumes that have hard links: one
//! `NtQueryInformationByName` per file relative to the directory handle, which the raw blocks
//! share, so chunk tasks spread these calls over the workers as `statx` is on Linux. A file
//! whose count is above one takes its sizes from that call too, as NTFS updates the directory
//! entry only of the name a file was changed through. The file id and the volume serial number
//! identify the file, so hard links are counted once as on Unix. Directories take the same call
//! for their allocation, which the enumeration reports as 0; symlinks and junctions need none.
//!
//! Reparse points whose tag is a name surrogate (symlinks, junctions, volume mount points) are
//! links: listed with their own size and never entered, so a scan stays on its volume except
//! for a scan root given as a link, which is followed. Cloud placeholders and offline files
//! (`WinStat::recalls_on_open`) are never opened: their sizes are the enumeration's (0 on disk
//! for `OneDrive`'s online-only files), and a placeholder directory not yet listed is not
//! entered. A directory on another volume is a mount root (its `dev`, the volume serial
//! number, differs), as on other systems.
//!
//! With [`WindowsBackend::with_mft`] (administrators only), each NTFS volume's master file
//! table is read once, in large sequential reads of the volume, and directories are listed from
//! it ([`super::ntfs`]) without opening them: names, sizes, attributes, reparse tags and link
//! counts are all in the file records. Each job root reads the table again, so a rescan sees
//! the volume as it is. Volumes whose table cannot be read (other filesystems, no
//! administrator rights) are listed as above.
//!
//! Directories are opened by `\\?\` path (`dfdu_sys::verbatim_path`), so paths of any length
//! up to the 32,767 characters Windows allows are read, and names ending in a dot or a space
//! are kept as they are.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use dfdu_sys::{DirReader, LinkQuery, Volume};
use dfdu_tree::{ErrKind, Kind, Meta, WinStat};

use super::ntfs::{Boot, MftBuilder, MftIndex};
use super::{Backend, Block, DirStream, PortableBackend, Raw, RawBlock, portable_queries};

#[derive(Default, Debug)]
pub struct WindowsBackend {
    portable: PortableBackend,
    /// Shared by every directory: records whether `NtQueryInformationByName` works.
    links: Arc<LinkQuery>,
    /// Set with [`WindowsBackend::with_mft`].
    mft: Option<Arc<MftCache>>,
}

/// A volume's file table as read, or why it could not be, and when it was read.
type Table = Arc<OnceLock<(std::time::Instant, Result<Arc<MftIndex>, Arc<io::Error>>)>>;

/// A table read this recently is not read again for a new job (the check before a scan).
const FRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// The tables of every backend with [`WindowsBackend::with_mft`], so the table read by
/// [`check_mft`] serves the scan.
static TABLES: std::sync::LazyLock<Arc<MftCache>> = std::sync::LazyLock::new(Arc::default);

/// Master file tables read, by volume serial number.
#[derive(Default)]
struct MftCache {
    vols: Mutex<std::collections::HashMap<u32, Table>>,
}

impl std::fmt::Debug for MftCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MftCache")
    }
}

impl MftCache {
    /// The table of volume `serial`, which `path` is on, read on first use; other workers
    /// wanting it wait for that read.
    fn get(&self, serial: u32, path: &Path) -> Result<Arc<MftIndex>, Arc<io::Error>> {
        let slot = self
            .vols
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(serial)
            .or_default()
            .clone();
        slot.get_or_init(|| {
            let t = read_mft(path).map(Arc::new).map_err(Arc::new);
            (std::time::Instant::now(), t)
        })
        .1
        .clone()
    }

    /// Reads volume `serial`'s table again on its next use, unless it was read just now.
    fn forget(&self, serial: u32) {
        let mut vols = self
            .vols
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if vols
            .get(&serial)
            .and_then(|t| t.get())
            .is_some_and(|(at, _)| at.elapsed() > FRESH)
        {
            vols.remove(&serial);
        }
    }
}

/// Bytes read from the volume at once.
const MFT_READ: usize = 4 << 20;

/// Reads the master file table of the NTFS volume `path` is on.
fn read_mft(path: &Path) -> io::Result<MftIndex> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::windows::fs::OpenOptionsExt;
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let dev = dfdu_sys::volume_device(path)?;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE: the volume stays in use.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(&dev)?;
    // Reads of a volume must be whole sectors: 4096 bytes cover every sector size.
    let mut boot = vec![0u8; 4096];
    f.read_exact(&mut boot)?;
    let b = Boot::parse(&boot).ok_or_else(|| bad("not an NTFS volume"))?;
    let (rs, cluster) = (b.record as usize, u64::from(b.cluster));
    let mut first = vec![0u8; rs.max(b.sector as usize)];
    f.seek(SeekFrom::Start(
        b.mft_lcn
            .checked_mul(cluster)
            .ok_or_else(|| bad("boot sector"))?,
    ))?;
    f.read_exact(&mut first)?;
    let rec0 = &mut first[..rs];
    if !super::ntfs::fixup(rec0) {
        return Err(bad("the $MFT record is damaged"));
    }
    let (runs, size) =
        super::ntfs::mft_data(rec0).ok_or_else(|| bad("the $MFT stream cannot be read"))?;
    let records = size / rs as u64;
    let mut builder = MftBuilder::default();
    let mut n = 0u64;
    // A record can span two runs when clusters are smaller than records.
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; MFT_READ];
    for run in runs {
        let mut left = run
            .len
            .checked_mul(cluster)
            .ok_or_else(|| bad("$MFT runs"))?;
        let Some(lcn) = run.lcn else {
            let left = usize::try_from(left).map_err(|_| bad("$MFT runs"))?;
            pending.resize(pending.len() + left, 0);
            continue;
        };
        f.seek(SeekFrom::Start(
            lcn.checked_mul(cluster).ok_or_else(|| bad("$MFT runs"))?,
        ))?;
        while left > 0 && n < records {
            let len = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
            f.read_exact(&mut buf[..len])?;
            left -= len as u64;
            pending.extend_from_slice(&buf[..len]);
            let whole = pending.len() / rs * rs;
            for rec in pending[..whole].chunks_exact_mut(rs) {
                if n < records {
                    builder.add(n, rec);
                }
                n += 1;
            }
            pending.drain(..whole);
        }
    }
    Ok(builder.finish())
}

/// A directory to read: its verbatim path, and whether a link at that path is followed (only
/// for a scan root).
#[derive(Clone, Debug)]
pub struct WinLoc {
    path: PathBuf,
    follow: bool,
    /// Listed from the file table: the volume serial number and the directory's file
    /// reference.
    mft: Option<(u32, u64)>,
}

impl WinLoc {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An open directory, shared by its stream and its raw blocks.
struct Dir {
    handle: OwnedHandle,
    path: PathBuf,
    volume: Volume,
    links: Arc<LinkQuery>,
}

pub struct WindowsStream(Stream);

enum Stream {
    Listed(Listed),
    Mft(MftList),
}

/// A directory read by enumeration.
struct Listed {
    dir: Arc<Dir>,
    reader: DirReader,
    pending_err: Option<io::Error>,
    done: bool,
}

/// A directory listed from its volume's file table.
struct MftList {
    index: Arc<MftIndex>,
    /// The directory's file reference.
    dir: u64,
    path: PathBuf,
    serial: u32,
    /// Entries handed out.
    at: usize,
}

/// Entries of a [`MftList`], by position.
struct MftRaw {
    index: Arc<MftIndex>,
    dir: u64,
    path: Arc<PathBuf>,
    serial: u32,
    range: std::ops::Range<usize>,
}

impl RawBlock<WinLoc> for MftRaw {
    fn len(&self) -> usize {
        self.range.len()
    }

    fn stat(&mut self, out: &mut Block<WinLoc>) -> io::Result<()> {
        let entries = self.index.children(self.dir).unwrap_or(&[]);
        let mut name8 = Vec::new();
        for e in entries.get(self.range.clone()).unwrap_or(&[]) {
            let name = self.index.name(e);
            name8.clear();
            dfdu_tree::name_bytes_wide(name, &mut name8);
            let mut st = e.stat;
            st.volume = self.serial;
            let meta = Meta::from_windows(&st);
            let child = (meta.kind == Kind::Dir && !st.recalls_on_open()).then(|| WinLoc {
                path: self.path.join(OsString::from_wide(name)),
                follow: false,
                mft: u64::try_from(st.file_id).ok().map(|id| (self.serial, id)),
            });
            out.push(&name8, Ok(meta), child);
        }
        self.range = 0..0;
        Ok(())
    }
}

/// Entries read from the enumeration buffer: UTF-16 names in one buffer, and each entry's
/// name span and metadata.
struct WinRaw {
    dir: Arc<Dir>,
    names: Vec<u16>,
    entries: Vec<(u32, u32, WinStat)>,
}

impl RawBlock<WinLoc> for WinRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<WinLoc>) -> io::Result<()> {
        let mut name8 = Vec::new();
        for &(off, len, mut st) in &self.entries {
            let name = &self.names[off as usize..(off + len) as usize];
            name8.clear();
            dfdu_tree::name_bytes_wide(name, &mut name8);
            st.volume = self.dir.volume.serial;
            let kind = st.kind();
            let mut meta = Ok(Meta::from_windows(&st));
            if kind == Kind::File && self.dir.volume.hard_links && !st.recalls_on_open() {
                let reparse = st.attributes & dfdu_tree::win_attr::REPARSE_POINT != 0;
                match self
                    .dir
                    .links
                    .query(self.dir.handle.as_handle(), name, reparse)
                {
                    Ok(l) => {
                        st.nlink = l.nlink;
                        st.size = l.size;
                        st.alloc = l.alloc;
                        meta = Ok(Meta::from_windows(&st));
                    }
                    // Listed but not readable: counted from the listing, as a single link.
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
                    Err(e) => meta = Err(ErrKind::from(&e)),
                }
            } else if kind == Kind::Dir && !st.recalls_on_open() {
                // The listing reports 0 for a directory; its index takes clusters once it no
                // longer fits in the MFT record. Unreadable: counted as listed.
                let reparse = st.attributes & dfdu_tree::win_attr::REPARSE_POINT != 0;
                if let Ok(l) = self
                    .dir
                    .links
                    .query(self.dir.handle.as_handle(), name, reparse)
                {
                    st.alloc = l.alloc;
                    meta = Ok(Meta::from_windows(&st));
                }
            }
            let child = match &meta {
                Ok(m) if m.kind == Kind::Dir && !st.recalls_on_open() => Some(WinLoc {
                    path: self.dir.path.join(OsString::from_wide(name)),
                    follow: false,
                    mft: None,
                }),
                _ => None,
            };
            out.push(&name8, meta, child);
        }
        self.entries.clear();
        Ok(())
    }
}

impl DirStream for WindowsStream {
    type Loc = WinLoc;

    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<WinLoc>, bool)> {
        match &mut self.0 {
            Stream::Listed(l) => l.next_raw(max_entries, max_name_bytes),
            Stream::Mft(m) => {
                let entries = m.index.children(m.dir).unwrap_or(&[]);
                let start = m.at;
                let mut names = 0;
                while m.at < entries.len()
                    && m.at - start < max_entries.max(1)
                    && names < max_name_bytes
                {
                    names += 2 * m.index.name(&entries[m.at]).len();
                    m.at += 1;
                }
                let raw = MftRaw {
                    index: m.index.clone(),
                    dir: m.dir,
                    path: Arc::new(m.path.clone()),
                    serial: m.serial,
                    range: start..m.at,
                };
                Ok((Box::new(raw), m.at < entries.len()))
            }
        }
    }
}

impl Listed {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a block holds at most `Limits::block_name_bytes` of names, far below `u32::MAX`"
    )]
    fn next_raw(
        &mut self,
        max_entries: usize,
        max_name_bytes: usize,
    ) -> io::Result<(Raw<WinLoc>, bool)> {
        if let Some(e) = self.pending_err.take() {
            self.done = true;
            return Err(e);
        }
        let mut raw = WinRaw {
            dir: self.dir.clone(),
            names: Vec::new(),
            entries: Vec::new(),
        };
        while !self.done && raw.entries.len() < max_entries && raw.names.len() * 2 < max_name_bytes
        {
            match self.reader.next(self.dir.handle.as_handle()) {
                Ok(None) => self.done = true,
                Err(e) => {
                    if raw.entries.is_empty() {
                        self.done = true;
                        return Err(e);
                    }
                    self.pending_err = Some(e);
                    break;
                }
                Ok(Some(e)) => {
                    let off = raw.names.len() as u32;
                    raw.names.extend(e.name_units());
                    let len = raw.names.len() as u32 - off;
                    raw.entries.push((off, len, e.stat));
                }
            }
        }
        Ok((Box::new(raw), !self.done))
    }
}

impl WindowsBackend {
    /// Lists NTFS volumes from their master file tables (needs administrator rights; other
    /// volumes are listed by enumeration).
    #[must_use]
    pub fn with_mft() -> WindowsBackend {
        WindowsBackend {
            mft: Some(TABLES.clone()),
            ..WindowsBackend::default()
        }
    }
}

/// Reads the file table of `path`'s volume; the table is kept for a scan starting right after.
///
/// # Errors
///
/// Why the table cannot be read: `path` cannot be stat'ed, its volume is not NTFS, the volume
/// cannot be opened (administrator rights are needed) or its table is damaged.
pub fn check_mft(path: &Path) -> io::Result<()> {
    let st = dfdu_sys::stat_path(path, true)?;
    TABLES
        .get(st.volume, path)
        .map(|_| ())
        .map_err(|e| io::Error::new(e.kind(), e.to_string()))
}

impl Backend for WindowsBackend {
    type Loc = WinLoc;
    type Stream = WindowsStream;

    fn read_prefix(&self, dir: &Self::Loc, name: &[u8], len: usize) -> Option<Vec<u8>> {
        super::read_prefix_at(dir.path(), name, len)
    }

    fn link_target_missing(&self, dir: &Self::Loc, name: &[u8]) -> bool {
        super::link_target_missing_at(dir.path(), name)
    }

    fn loc_from_path(&self, path: &Path) -> WinLoc {
        WinLoc {
            path: dfdu_sys::verbatim_path(path).unwrap_or_else(|_| path.to_path_buf()),
            follow: true,
            mft: None,
        }
    }

    fn stat_root(&self, loc: &WinLoc) -> io::Result<Meta> {
        let st = dfdu_sys::stat_path(&loc.path, true)?;
        // A new job: its volume's table is read again, so it lists what is there now.
        if let Some(c) = &self.mft {
            c.forget(st.volume);
        }
        Ok(Meta::from_windows(&st))
    }

    fn stat_dir(&self, path: &Path) -> io::Result<Meta> {
        // Not a new job: the volume's table is kept.
        let path = dfdu_sys::verbatim_path(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Meta::from_windows(&dfdu_sys::stat_path(&path, false)?))
    }

    fn open(&self, loc: &WinLoc, _storage: dfdu_sys::Storage) -> io::Result<WindowsStream> {
        if let Some(cache) = &self.mft {
            let at = match loc.mft {
                Some(at) => Some(at),
                // A job root: where it is in the table.
                None => dfdu_sys::stat_path(&loc.path, loc.follow)
                    .ok()
                    .filter(|st| st.kind() == Kind::Dir && !st.recalls_on_open())
                    // A reference wider than 64 bits is not one in an NTFS file table.
                    .and_then(|st| Some((st.volume, u64::try_from(st.file_id).ok()?))),
            };
            if let Some((serial, dir)) = at
                && let Ok(index) = cache.get(serial, &loc.path)
                && index.children(dir).is_some()
            {
                return Ok(WindowsStream(Stream::Mft(MftList {
                    index,
                    dir,
                    path: loc.path.clone(),
                    serial,
                    at: 0,
                })));
            }
            // Not in the table read (created since, or another filesystem): listed.
        }
        let handle = dfdu_sys::open_dir(&loc.path, loc.follow)?;
        let volume = dfdu_sys::volume_of(handle.as_handle())?;
        Ok(WindowsStream(Stream::Listed(Listed {
            dir: Arc::new(Dir {
                handle,
                path: loc.path.clone(),
                volume,
                links: self.links.clone(),
            }),
            reader: DirReader::default(),
            pending_err: None,
            done: false,
        })))
    }

    portable_queries!(loc => &loc.path);
}

#[cfg(test)]
mod tests;
