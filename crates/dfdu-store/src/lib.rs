//! Persistence: ncdu JSON import and export, native snapshots, the scan cache, scan history,
//! zstd streams and atomic file writes.

pub mod cache;
pub mod history;
pub mod ncdu;
pub mod snapshot;

use std::fmt;
use std::io;

/// Why an import stopped.
#[derive(Debug)]
pub enum ImportError {
    Io(io::Error),
    /// Not valid input: what was wrong and, when known, where, in bytes from the start (of the
    /// decompressed stream).
    Format {
        offset: Option<u64>,
        msg: String,
    },
    /// Past one of the import's limits.
    Limit(String),
}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        ImportError::Io(e)
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Io(e) => write!(f, "{e}"),
            ImportError::Format {
                offset: Some(offset),
                msg,
            } => write!(f, "at byte {offset}: {msg}"),
            ImportError::Format { offset: None, msg } => write!(f, "{msg}"),
            ImportError::Limit(msg) => write!(f, "over a limit: {msg}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Limits on what an import accepts, so a hostile or broken file cannot use unbounded memory
/// or stack.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Directory nesting below the root.
    pub max_depth: usize,
    /// Bytes in a name (the default is ncdu's own limit).
    pub max_name: usize,
    pub max_entries: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 4096,
            max_name: 32_768,
            max_entries: 200_000_000,
        }
    }
}

/// Compresses what is written to it with zstd, as a series of frames of at most [`ZSTD_FRAME`]
/// bytes each (zstd readers read concatenated frames as one stream). Encoding into memory frame
/// by frame keeps write errors as errors: the encoder panics on them. `flush` ends the current
/// frame early. Call [`ZstdWriter::finish`] at the end.
pub struct ZstdWriter<W: io::Write> {
    inner: W,
    buf: Vec<u8>,
}

/// Uncompressed bytes per zstd frame.
pub const ZSTD_FRAME: usize = 1 << 20;

impl<W: io::Write> ZstdWriter<W> {
    pub fn new(w: W) -> Self {
        ZstdWriter {
            inner: w,
            buf: Vec::with_capacity(ZSTD_FRAME),
        }
    }

    fn frame(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let z = ruzstd::encoding::compress_to_vec(
                &self.buf[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            self.inner.write_all(&z)?;
            self.buf.clear();
        }
        Ok(())
    }

    /// Writes the last frame and flushes.
    ///
    /// # Errors
    ///
    /// When the inner writer fails to write or flush.
    pub fn finish(mut self) -> io::Result<W> {
        self.frame()?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: io::Write> io::Write for ZstdWriter<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = b.len().min(ZSTD_FRAME - self.buf.len());
        self.buf.extend_from_slice(&b[..n]);
        if self.buf.len() == ZSTD_FRAME {
            self.frame()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.frame()?;
        self.inner.flush()
    }
}

/// Reads a series of zstd frames as one stream.
pub struct ZstdReader<R: io::BufRead> {
    src: Option<R>,
    frame: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
}

impl<R: io::BufRead> ZstdReader<R> {
    pub fn new(r: R) -> Self {
        ZstdReader {
            src: Some(r),
            frame: None,
        }
    }
}

impl<R: io::BufRead> io::Read for ZstdReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(d) = self.frame.as_mut() {
                let n = d.read(out)?;
                if n > 0 || out.is_empty() {
                    return Ok(n);
                }
                // The frame ended; another may follow.
                if let Some(d) = self.frame.take() {
                    self.src = Some(d.into_inner());
                }
            }
            // A frame that failed to start took the source with it.
            let Some(mut src) = self.src.take() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "read past a bad zstd frame",
                ));
            };
            if src.fill_buf()?.is_empty() {
                self.src = Some(src);
                return Ok(0);
            }
            let d = ruzstd::decoding::StreamingDecoder::new(src)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            self.frame = Some(d);
        }
    }
}

/// Writes `path` all at once or not at all: into a temporary file next to it, synced, then
/// renamed over it, and the directory synced, so a crash leaves the old file or the new one.
/// The temporary file is locked while it is written; those of writers that died (unlocked)
/// are removed first.
///
/// # Errors
///
/// `InvalidInput` when `path` has no file name, the error of `f`, and any error creating,
/// syncing or renaming the temporary file or syncing the directory. The temporary file is
/// removed when `f`, the sync or the rename fails.
pub fn write_atomic<T>(
    path: &std::path::Path,
    f: impl FnOnce(&mut std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    replace_file(path, true, f)
}

/// [`write_atomic`] without the syncs, for a file whose loss costs nothing but time (the scan
/// cache). A process that dies leaves the old file or the new one; a system crash or power loss
/// can also leave a new one cut short or empty under its name, which its reader must refuse
/// (a snapshot's checksums do, and the scan cache then scans again).
///
/// # Errors
///
/// As [`write_atomic`].
pub fn write_replacing<T>(
    path: &std::path::Path,
    f: impl FnOnce(&mut std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    replace_file(path, false, f)
}

fn replace_file<T>(
    path: &std::path::Path,
    sync: bool,
    f: impl FnOnce(&mut std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => std::path::Path::new("."),
    };
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut prefix = std::ffi::OsString::from(".");
    prefix.push(name);
    prefix.push(".");
    remove_stale_temps(dir, prefix.as_encoded_bytes());
    let mut tmp_name = prefix;
    tmp_name.push(format!("{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let mut file = std::fs::File::create(&tmp)?;
    // Held until the file is closed, by this process or by its death.
    let _ = file.try_lock();
    let r = f(&mut file).and_then(|v| {
        if sync {
            file.sync_all()?;
        }
        Ok(v)
    });
    let v = match r {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    drop(file);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Windows cannot open a directory to sync it; the rename is durable there once done.
    #[cfg(unix)]
    if sync {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(v)
}

/// Removes the temporary files `<prefix><pid>.tmp` in `dir` that no writer holds locked: those
/// of writers killed before they renamed them.
fn remove_stale_temps(dir: &std::path::Path, prefix: &[u8]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name();
        let b = n.as_encoded_bytes();
        let Some(pid) = b.strip_prefix(prefix).and_then(|r| r.strip_suffix(b".tmp")) else {
            continue;
        };
        if pid.is_empty() || !pid.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Ok(f) = std::fs::OpenOptions::new().write(true).open(e.path()) else {
            continue;
        };
        if f.try_lock().is_ok() {
            drop(f);
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A scan read from a file of either kind.
pub struct Opened {
    pub tree: dfdu_tree::Tree,
    pub root: dfdu_tree::Ix,
    pub entries: u64,
    /// The program that wrote it and its version, when the file says.
    pub written_by: Option<String>,
    /// What the scan was (snapshots only): its mount options, and where the change journal
    /// stood when it started.
    pub header: Option<snapshot::Header>,
}

/// Reads a dfdu snapshot or an ncdu export (JSON, plain or zstd, or binary), told apart by
/// their first bytes. A binary export is read into memory first, since its reader seeks;
/// [`open_file`] reads one from the file in place.
///
/// # Errors
///
/// When `r` fails, the data is in neither format or malformed, or it exceeds `limits`.
pub fn open(r: impl io::Read, limits: &Limits) -> Result<Opened, ImportError> {
    let mut r = io::BufReader::with_capacity(1 << 16, r);
    let head = io::BufRead::fill_buf(&mut r)?;
    if ncdu::binary::is_binary(head) {
        let mut all = Vec::new();
        io::Read::read_to_end(&mut r, &mut all)?;
        return Ok(binary_opened(ncdu::binary::import(
            io::Cursor::new(all),
            limits,
        )?));
    }
    if snapshot::is_snapshot(head) {
        let l = snapshot::read(r, limits)?;
        return Ok(Opened {
            tree: l.tree,
            root: l.root,
            entries: l.entries,
            written_by: Some(format!("dfdu {}", l.header.tool_version)),
            header: Some(l.header),
        });
    }
    let im = ncdu::import(r, limits)?;
    let written_by = match (im.meta.progname, im.meta.progver) {
        (Some(n), Some(v)) => Some(format!("{n} {v}")),
        (n, _) => n,
    };
    Ok(Opened {
        tree: im.tree,
        root: im.root,
        entries: im.entries,
        written_by,
        header: None,
    })
}

/// [`open`] for a file, reading a binary export without loading it whole.
///
/// # Errors
///
/// As [`open`], and when the file cannot be read or rewound.
pub fn open_file(mut f: std::fs::File, limits: &Limits) -> Result<Opened, ImportError> {
    let mut head = [0u8; 8];
    let n = io::Read::read(&mut f, &mut head)?;
    io::Seek::rewind(&mut f)?;
    if ncdu::binary::is_binary(&head[..n]) {
        return Ok(binary_opened(ncdu::binary::import(
            io::BufReader::new(f),
            limits,
        )?));
    }
    open(f, limits)
}

fn binary_opened(im: ncdu::Imported) -> Opened {
    Opened {
        tree: im.tree,
        root: im.root,
        entries: im.entries,
        written_by: Some("ncdu (binary export)".into()),
        header: None,
    }
}

#[cfg(test)]
mod tests;
