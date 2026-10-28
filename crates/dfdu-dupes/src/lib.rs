//! Duplicate files below a scanned directory, found on request.
//!
//! [`candidates`] picks the files from the tree under its read lock: regular files of at least
//! the minimum size, each inode once (hard links are one file, not duplicates), and not cloud
//! placeholders, whose data would be downloaded by reading them (no allocation for a file with
//! data, or `SF_DATALESS` on macOS). [`find`] then reads them without any lock, in three rounds
//! that each drop the files left alone in their group: equal size, then an xxh3-128 hash of the
//! first and last [`PARTIAL`] bytes, then a SHA-256 of the whole file. Files are opened without
//! following symlinks and without blocking, and one whose size or inode changed since the scan
//! is left out.
//!
//! On APFS, copies are often clones that share their data (`cp` and the Finder make them), so
//! deleting one frees only what it does not share: each file's private size is read when its
//! contents are hashed, and [`Group::wasted`] counts that. On Linux the same comes from the
//! extent map: extents FIEMAP marks shared (btrfs and XFS reflinks, snapshots) are not counted
//! as freed.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use dfdu_tree::{Ix, Kind, Tree};
use sha2::{Digest, Sha256};

/// Bytes hashed at each end of a file in the partial round.
pub const PARTIAL: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Options {
    /// Files smaller than this are not compared.
    pub min_size: u64,
    /// Threads reading files.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_size: 1 << 20,
            threads: 4,
        }
    }
}

/// A file that may have duplicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// Length in bytes.
    pub size: u64,
    /// Bytes allocated on disk.
    pub alloc: u64,
    pub dev_ino: (u64, u64),
    pub mtime: i64,
    /// Other names of the same inode in the scan (hard links), not counted as duplicates.
    pub links: u32,
    /// Bytes the file shares with no other file (APFS, or shared extents on Linux), read by
    /// [`find`]; `None` when it shares nothing or the filesystem does not say.
    pub private: Option<u64>,
}

impl Candidate {
    /// What deleting this name alone frees: nothing for a file with other names, its private
    /// bytes when known, its allocation otherwise.
    pub fn freed(&self) -> u64 {
        if self.links > 0 {
            0
        } else {
            self.private.unwrap_or(self.alloc)
        }
    }
}

/// Files with the same contents.
#[derive(Clone, Debug)]
pub struct Group {
    pub size: u64,
    /// The SHA-256 of the contents.
    pub hash: [u8; 32],
    /// In path order.
    pub files: Vec<Candidate>,
}

impl Group {
    /// What deleting every copy but one would free at most: all of them but the one whose
    /// deletion frees least ([`Candidate::freed`]). Clones of one another free nothing.
    pub fn wasted(&self) -> u64 {
        let total: u64 = self.files.iter().map(Candidate::freed).sum();
        total - self.files.iter().map(Candidate::freed).min().unwrap_or(0)
    }

    /// Every copy shares its data with the others (APFS clones, reflinks): deleting copies frees
    /// nothing.
    pub fn all_clones(&self) -> bool {
        self.files.iter().all(|f| f.private == Some(0))
    }
}

/// Counters for a progress line, updated while [`find`] runs.
#[derive(Debug, Default)]
pub struct Progress {
    /// Files and bytes the current round reads in all.
    pub round_files: AtomicU64,
    pub round_bytes: AtomicU64,
    /// Read so far in the current round.
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    /// 1: partial hashes, 2: full hashes.
    pub round: AtomicU64,
    /// Files left out because they could not be read or changed since the scan.
    pub skipped: AtomicU64,
}

/// What [`find`] found.
#[derive(Debug, Default)]
pub struct Found {
    /// Largest waste first.
    pub groups: Vec<Group>,
    /// Files compared (after the size round).
    pub compared: u64,
    /// Files left out because they could not be read or changed since the scan.
    pub skipped: u64,
    /// The search was cancelled; `groups` is empty.
    pub cancelled: bool,
}

/// The files at or below `root` worth comparing: at least `min_size` bytes, one per inode,
/// not cloud placeholders, and only sizes that more than one file has.
pub fn candidates(tree: &Tree, root: Ix, min_size: u64) -> Vec<Candidate> {
    let mut by_inode: HashMap<(u64, u64), usize> = HashMap::new();
    let mut out: Vec<Candidate> = Vec::new();
    let mut add = |c: Ix| {
        if tree.kind(c) != Kind::File {
            return;
        }
        let own = tree.own(c);
        if own.apparent < min_size.max(1) {
            return;
        }
        let dev_ino = tree.dev_ino(c);
        // Placeholders without a file id (Windows) are never the same file as another.
        if dev_ino.1 != 0
            && let Some(&i) = by_inode.get(&dev_ino)
        {
            // Only one name of a hard-linked file is charged with its allocation.
            out[i].alloc = out[i].alloc.max(own.alloc);
            out[i].links += 1;
            return;
        }
        by_inode.insert(dev_ino, out.len());
        out.push(Candidate {
            path: tree.path_buf(c),
            size: own.apparent,
            alloc: own.alloc,
            dev_ino,
            mtime: tree.mtime(c),
            links: 0,
            private: None,
        });
    };
    if tree.dir(root).is_none() {
        add(root);
    } else {
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            for c in tree.children(d) {
                if tree.dir(c).is_some() {
                    stack.push(c);
                } else {
                    add(c);
                }
            }
        }
    }
    // Nothing allocated for a file with data: a placeholder whose data is elsewhere.
    out.retain(|f| f.alloc > 0);
    let mut sizes: HashMap<u64, u32> = HashMap::new();
    for f in &out {
        *sizes.entry(f.size).or_default() += 1;
    }
    out.retain(|f| sizes[&f.size] > 1);
    out
}

/// Opens `path` for reading if it is still the regular file the scan saw: not a symlink, a
/// FIFO or a device (never followed or blocked on), with the same length and inode, and not a
/// macOS dataless file.
fn open_checked(c: &Candidate) -> Option<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(&c.path).ok()?;
    let m = f.metadata().ok()?;
    if !m.is_file() || m.len() != c.size {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if c.dev_ino.1 != 0 && m.ino() != c.dev_ino.1 {
            return None;
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const SF_DATALESS: u32 = 0x4000_0000;
        if m.st_flags() & SF_DATALESS != 0 {
            return None;
        }
    }
    Some(f)
}

/// The partial hash: the first and last [`PARTIAL`] bytes (the whole file when it is smaller
/// than twice that).
fn partial_hash(c: &Candidate) -> Option<u128> {
    let mut f = open_checked(c)?;
    let mut buf = Vec::with_capacity(2 * PARTIAL);
    if c.size <= 2 * PARTIAL as u64 {
        f.read_to_end(&mut buf).ok()?;
    } else {
        buf.resize(2 * PARTIAL, 0);
        f.read_exact(&mut buf[..PARTIAL]).ok()?;
        f.seek(SeekFrom::Start(c.size - PARTIAL as u64)).ok()?;
        f.read_exact(&mut buf[PARTIAL..]).ok()?;
    }
    (buf.len() as u64 == c.size.min(2 * PARTIAL as u64))
        .then(|| twox_hash::XxHash3_128::oneshot(&buf))
}

/// The private size of the open file: from APFS's clone information, or on Linux from its
/// extent map when some extents are shared (btrfs and XFS reflinks, snapshots). `None` when
/// the file shares nothing or the filesystem does not say.
fn private_size(f: &File) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsFd;
        dfdu_sys::clone_info(f.as_fd()).map(|i| i.private_size)
    }
    #[cfg(target_os = "linux")]
    {
        use dfdu_sys::Extent;
        let ext = dfdu_sys::fiemap(f).ok()?;
        if !ext.iter().any(|e| e.flags & Extent::SHARED != 0) {
            return None;
        }
        let alloc = {
            use std::os::unix::fs::MetadataExt;
            f.metadata().ok()?.blocks() * 512
        };
        let own: u64 = ext
            .iter()
            .filter(|e| e.flags & (Extent::SHARED | Extent::DATA_INLINE) == 0)
            .map(|e| e.length)
            .sum();
        Some(own.min(alloc))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = f;
        None
    }
}

/// The SHA-256 of the whole file and its private size; `None` if it could not be read or its
/// length changed.
fn full_hash(
    c: &Candidate,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Option<([u8; 32], Option<u64>)> {
    let mut f = open_checked(c)?;
    let private = private_size(&f);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut read = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        h.update(&buf[..n]);
        read += n as u64;
        progress.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    (read == c.size).then(|| (h.finalize().into(), private))
}

/// Runs `f` on every item on `threads` threads; `None` results are dropped.
fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    cancel: &AtomicBool,
    f: impl Fn(&T) -> Option<R> + Sync,
) -> Vec<(usize, R)> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, items.len().max(1)) {
            s.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(r) = f(&items[i]) {
                        mine.push((i, r));
                    }
                }
                out.lock().unwrap_or_else(|e| e.into_inner()).extend(mine);
            });
        }
    });
    let mut v = out.into_inner().unwrap_or_else(|e| e.into_inner());
    v.sort_unstable_by_key(|r| r.0);
    v
}

/// Keeps the entries whose key more than one entry has.
fn shared_keys<K: std::hash::Hash + Eq + Copy>(v: Vec<(K, Candidate)>) -> Vec<(K, Candidate)> {
    let mut n: HashMap<K, u32> = HashMap::new();
    for (k, _) in &v {
        *n.entry(*k).or_default() += 1;
    }
    v.into_iter().filter(|(k, _)| n[k] > 1).collect()
}

/// Compares `cands` (from [`candidates`]) and returns the groups of identical files.
/// `cancel` stops it between files and between reads.
pub fn find(
    cands: Vec<Candidate>,
    opts: &Options,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Found {
    let compared = cands.len() as u64;
    let start_round = |round: u64, files: &[Candidate], bytes: u64| {
        progress.round.store(round, Ordering::Relaxed);
        progress
            .round_files
            .store(files.len() as u64, Ordering::Relaxed);
        progress.round_bytes.store(bytes, Ordering::Relaxed);
        progress.files.store(0, Ordering::Relaxed);
        progress.bytes.store(0, Ordering::Relaxed);
    };
    let partial_bytes: u64 = cands.iter().map(|c| c.size.min(2 * PARTIAL as u64)).sum();
    start_round(1, &cands, partial_bytes);
    let partial = par_map(&cands, opts.threads, cancel, |c| {
        let h = partial_hash(c);
        progress.files.fetch_add(1, Ordering::Relaxed);
        progress
            .bytes
            .fetch_add(c.size.min(2 * PARTIAL as u64), Ordering::Relaxed);
        if h.is_none() {
            progress.skipped.fetch_add(1, Ordering::Relaxed);
        }
        h
    });
    if cancel.load(Ordering::Relaxed) {
        return Found {
            cancelled: true,
            ..Found::default()
        };
    }
    let mut by_partial: Vec<((u64, u128), Candidate)> = partial
        .into_iter()
        .map(|(i, h)| ((cands[i].size, h), cands[i].clone()))
        .collect();
    by_partial = shared_keys(by_partial);
    let second: Vec<Candidate> = by_partial.into_iter().map(|(_, c)| c).collect();
    start_round(2, &second, second.iter().map(|c| c.size).sum());
    let full = par_map(&second, opts.threads, cancel, |c| {
        let h = full_hash(c, progress, cancel);
        progress.files.fetch_add(1, Ordering::Relaxed);
        if h.is_none() && !cancel.load(Ordering::Relaxed) {
            progress.skipped.fetch_add(1, Ordering::Relaxed);
        }
        h
    });
    if cancel.load(Ordering::Relaxed) {
        return Found {
            cancelled: true,
            ..Found::default()
        };
    }
    let mut groups: HashMap<(u64, [u8; 32]), Vec<Candidate>> = HashMap::new();
    for (i, (h, private)) in full {
        let c = Candidate {
            private,
            ..second[i].clone()
        };
        groups.entry((c.size, h)).or_default().push(c);
    }
    let mut groups: Vec<Group> = groups
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|((size, hash), mut files)| {
            files.sort_by(|a, b| a.path.cmp(&b.path));
            Group { size, hash, files }
        })
        .collect();
    groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
    Found {
        groups,
        compared,
        skipped: progress.skipped.load(Ordering::Relaxed),
        cancelled: false,
    }
}

/// The hash as lower-case hex.
pub fn hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `path` is the file the candidate describes (its length and inode), for a caller
/// about to act on it.
pub fn still_same(c: &Candidate) -> bool {
    open_checked(c).is_some()
}

#[cfg(all(test, unix))]
mod tests;
