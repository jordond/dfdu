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
//! The record stream starts with the scan's header (root path, tool version, time, options)
//! and holds the tree depth first: a directory opens with its fields, its entries follow, and
//! a close record ends it. Numbers are LEB128 varints, signed ones zigzagged. Devices go into
//! a table as they first appear and are referred to by index after.
//!
//! Not kept: classes and protection (the rescan decides them again), btrfs subvolume records
//! and measured extents (likewise), errors' OS codes beyond the kind.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};

use dfdu_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, Tree};

use crate::ImportError;
use crate::ncdu::Limits;

pub const MAGIC: &[u8; 8] = b"DFDUSNAP";
pub const VERSION: u32 = 3;
/// Version 1, the same without the change journal position.
const VERSION_1: u32 = 1;
/// Version 2, the same without the time of the journal position.
const VERSION_2: u32 = 2;
/// Raw bytes per frame.
pub const FRAME_RAW: usize = 1 << 20;
/// Largest frame a reader accepts, raw or compressed.
const FRAME_MAX: usize = 16 << 20;

const DIR: u8 = 1;
const CLOSE: u8 = 2;
const ENTRY: u8 = 3;

/// Flags of a directory that are kept.
const DIR_FLAGS: [u16; 4] = [
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
    pub journal: Option<Journal>,
}

/// A position in a volume's change journal (FSEvents on macOS, the USN journal on Windows,
/// a btrfs subvolume's generation on Linux).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    /// The journal's identity (the FSEvents store UUID; the USN journal id and volume serial;
    /// the btrfs subvolume uuid):
    /// positions in another are meaningless.
    pub volume: [u8; 16],
    /// The last event before the scan (on btrfs, the subvolume's generation).
    pub event_id: u64,
    /// Seconds since the Unix epoch when the position was read, 0 when not known (format 2).
    /// btrfs resumes count an inode as changed only if its ctime is not older, so atime
    /// updates (the scan's own among them) do not.
    pub time: u64,
}

const OPT_JOURNAL: u64 = 4;

pub struct Loaded {
    pub tree: Tree,
    pub root: Ix,
    pub header: Header,
    pub entries: u64,
}

struct FrameWriter<W: Write> {
    w: W,
    raw: Vec<u8>,
    frames: u64,
    crcs: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    fn frame(&mut self) -> io::Result<()> {
        if self.raw.is_empty() {
            return Ok(());
        }
        let z = ruzstd::encoding::compress_to_vec(
            &self.raw[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let crc = crc32c::crc32c(&z);
        let mut h = [0u8; 12];
        h[..4].copy_from_slice(&(z.len() as u32).to_le_bytes());
        h[4..8].copy_from_slice(&(self.raw.len() as u32).to_le_bytes());
        h[8..].copy_from_slice(&crc.to_le_bytes());
        self.w.write_all(&h)?;
        self.w.write_all(&z)?;
        self.crcs.extend_from_slice(&crc.to_le_bytes());
        self.frames += 1;
        self.raw.clear();
        Ok(())
    }

    fn maybe_frame(&mut self) -> io::Result<()> {
        if self.raw.len() >= FRAME_RAW {
            self.frame()?;
        }
        Ok(())
    }
}

fn put_u(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_i(out: &mut Vec<u8>, v: i64) {
    put_u(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn err_code(e: Option<ErrKind>) -> (u8, i64) {
    match e {
        None => (0, 0),
        Some(ErrKind::PermissionDenied) => (1, 0),
        Some(ErrKind::NotFound) => (2, 0),
        Some(ErrKind::Stalled) => (3, 0),
        Some(ErrKind::Other) => (4, 0),
        Some(ErrKind::Os(c)) => (5, c as i64),
    }
}

fn state_code(s: DirState) -> u8 {
    match s {
        DirState::Pending => 0,
        DirState::Complete => 1,
        DirState::Partial => 2,
        DirState::Failed => 3,
        DirState::Skipped => 4,
        DirState::Cancelled => 5,
        DirState::Abandoned => 6,
        DirState::Lost => 7,
        DirState::Superseded => 8,
        DirState::Deleting => 9,
        DirState::Deleted => 10,
    }
}

/// Writes the tree below `root` (a visible root, whose name is its path).
pub fn write(tree: &Tree, root: Ix, w: impl Write, header: &Header) -> io::Result<u64> {
    write_as(tree, root, w, header, VERSION)
}

/// [`write`] in format `version` (2 or 3; tests write older formats with it).
fn write_as(
    tree: &Tree,
    root: Ix,
    w: impl Write,
    header: &Header,
    version: u32,
) -> io::Result<u64> {
    let mut fw = FrameWriter {
        w: io::BufWriter::with_capacity(1 << 16, w),
        raw: Vec::with_capacity(FRAME_RAW + 4096),
        frames: 0,
        crcs: vec![],
    };
    fw.w.write_all(MAGIC)?;
    fw.w.write_all(&version.to_le_bytes())?;
    let path = tree.path_buf(root);
    put_bytes(&mut fw.raw, &dfdu_tree::name_bytes(path.as_os_str()));
    put_bytes(&mut fw.raw, header.tool_version.as_bytes());
    put_u(&mut fw.raw, header.written);
    put_u(
        &mut fw.raw,
        header.cross_mounts as u64
            | (header.network as u64) << 1
            | if header.journal.is_some() {
                OPT_JOURNAL
            } else {
                0
            },
    );
    if let Some(j) = &header.journal {
        put_bytes(&mut fw.raw, &j.volume);
        put_u(&mut fw.raw, j.event_id);
        if version == VERSION {
            put_u(&mut fw.raw, j.time);
        }
    }
    let mut devs: Vec<u64> = vec![];
    let mut entries = 0u64;
    let mut stack: Vec<(Vec<Ix>, usize)> = vec![];
    let mut next = Some(root);
    loop {
        if let Some(ix) = next.take() {
            entries += 1;
            let r = &mut fw.raw;
            let is_dir = tree.dir(ix).is_some();
            r.push(if is_dir { DIR } else { ENTRY });
            put_bytes(r, if ix == root { b"" } else { tree.name(ix) });
            let flags = tree.flags(ix);
            if !is_dir {
                r.push(match tree.kind(ix) {
                    Kind::File => 0,
                    Kind::Dir => 1,
                    Kind::Symlink => 2,
                    Kind::Other => 3,
                });
                r.push(flags.has(NodeFlags::HARDLINK) as u8);
            }
            let (apparent, alloc) = tree.raw_size(ix);
            put_u(r, apparent);
            put_u(r, alloc);
            put_i(r, tree.mtime(ix));
            let (dev, ino) = tree.dev_ino(ix);
            match devs.iter().position(|&d| d == dev) {
                Some(i) => put_u(r, i as u64),
                None => {
                    put_u(r, devs.len() as u64);
                    put_u(r, dev);
                    devs.push(dev);
                }
            }
            put_u(r, ino);
            let (code, os) = err_code(tree.error(ix));
            r.push(code);
            if code == 5 {
                put_i(r, os);
            }
            if let Some(d) = tree.dir(ix) {
                let kept = DIR_FLAGS
                    .iter()
                    .filter(|&&f| flags.has(f))
                    .fold(0, |a, &f| a | f);
                put_u(r, u64::from(kept));
                r.push(state_code(d.state));
                stack.push((tree.children(ix).collect(), 0));
            }
            fw.maybe_frame()?;
        }
        let Some((kids, i)) = stack.last_mut() else {
            break;
        };
        match kids.get(*i) {
            Some(&c) => {
                *i += 1;
                next = Some(c);
            }
            None => {
                stack.pop();
                fw.raw.push(CLOSE);
            }
        }
    }
    fw.frame()?;
    fw.w.write_all(&[0; 12])?;
    let mut foot = Vec::with_capacity(24);
    foot.extend_from_slice(&entries.to_le_bytes());
    foot.extend_from_slice(&fw.frames.to_le_bytes());
    foot.extend_from_slice(&crc32c::crc32c(&fw.crcs).to_le_bytes());
    let fcrc = crc32c::crc32c(&foot);
    foot.extend_from_slice(&fcrc.to_le_bytes());
    fw.w.write_all(&foot)?;
    fw.w.flush()?;
    Ok(entries)
}

/// The frames' raw bytes as one stream, each frame checked as it is read. At the end marker it
/// reads the footer and checks it against what was read.
struct FrameReader<R: Read> {
    r: R,
    cur: Vec<u8>,
    pos: usize,
    frames: u64,
    crcs: Vec<u8>,
    /// Entries the footer counts, once it has been read.
    footer_entries: Option<u64>,
}

fn bad(msg: impl Into<String>) -> ImportError {
    ImportError::Format {
        offset: 0,
        msg: msg.into(),
    }
}

impl<R: Read> FrameReader<R> {
    fn next_frame(&mut self) -> Result<bool, ImportError> {
        if self.footer_entries.is_some() {
            return Ok(false);
        }
        let mut h = [0u8; 12];
        self.r.read_exact(&mut h).map_err(truncated)?;
        let u = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().expect("4 bytes")) as usize;
        let (zlen, rawlen, crc) = (u(0), u(4), u(8) as u32);
        if zlen == 0 {
            if h != [0; 12] {
                return Err(bad("a bad end marker"));
            }
            let mut foot = [0u8; 24];
            self.r.read_exact(&mut foot).map_err(truncated)?;
            let q = |i: usize| u64::from_le_bytes(foot[i..i + 8].try_into().expect("8 bytes"));
            let fcrc = u32::from_le_bytes(foot[20..].try_into().expect("4 bytes"));
            if crc32c::crc32c(&foot[..20]) != fcrc {
                return Err(bad("the footer is damaged"));
            }
            if q(8) != self.frames
                || u32::from_le_bytes(foot[16..20].try_into().expect("4 bytes"))
                    != crc32c::crc32c(&self.crcs)
            {
                return Err(bad("frames are missing or out of order"));
            }
            let mut rest = [0u8; 1];
            if self.r.read(&mut rest)? != 0 {
                return Err(bad("data after the footer"));
            }
            self.footer_entries = Some(q(0));
            return Ok(false);
        }
        if zlen > FRAME_MAX || rawlen > FRAME_MAX || rawlen == 0 {
            return Err(bad("a frame of impossible size"));
        }
        let mut z = vec![0u8; zlen];
        self.r.read_exact(&mut z).map_err(truncated)?;
        if crc32c::crc32c(&z) != crc {
            return Err(bad(format!("frame {} is damaged", self.frames)));
        }
        let mut d = ruzstd::decoding::StreamingDecoder::new(&z[..])
            .map_err(|e| bad(format!("frame {}: {e}", self.frames)))?;
        self.cur.clear();
        (&mut d)
            .take(rawlen as u64 + 1)
            .read_to_end(&mut self.cur)
            .map_err(|e| bad(format!("frame {}: {e}", self.frames)))?;
        if self.cur.len() != rawlen {
            return Err(bad(format!("frame {} has the wrong length", self.frames)));
        }
        self.pos = 0;
        self.frames += 1;
        self.crcs.extend_from_slice(&crc.to_le_bytes());
        Ok(true)
    }
}

fn truncated(e: io::Error) -> ImportError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        bad("the snapshot is cut short")
    } else {
        ImportError::Io(e)
    }
}

impl<R: Read> Read for FrameReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.cur.len() {
            match self.next_frame() {
                Ok(true) => {}
                Ok(false) => return Ok(0),
                Err(ImportError::Io(e)) => return Err(e),
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct Rec<R: BufRead>(R);

impl<R: BufRead> Rec<R> {
    fn byte(&mut self) -> Result<u8, ImportError> {
        let mut b = [0u8; 1];
        self.0.read_exact(&mut b).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => bad("the records end early"),
            io::ErrorKind::InvalidData => bad(e.to_string()),
            _ => ImportError::Io(e),
        })?;
        Ok(b[0])
    }

    fn u(&mut self) -> Result<u64, ImportError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("a number too long"))
    }

    fn i(&mut self) -> Result<i64, ImportError> {
        let u = self.u()?;
        Ok((u >> 1) as i64 ^ -((u & 1) as i64))
    }

    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, ImportError> {
        let n = self.u()?;
        if n > max as u64 {
            return Err(ImportError::Limit(format!(
                "a name longer than {max} bytes"
            )));
        }
        let mut v = vec![0u8; n as usize];
        for b in v.iter_mut() {
            *b = self.byte()?;
        }
        Ok(v)
    }
}

/// Whether `bytes` start like a snapshot.
pub fn is_snapshot(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Reads a snapshot. Anything damaged, cut short or past `limits` is an error, and nothing of
/// it is used.
pub fn read(r: impl Read, limits: &Limits) -> Result<Loaded, ImportError> {
    let mut r = BufReader::with_capacity(1 << 16, r);
    let mut head = [0u8; 12];
    r.read_exact(&mut head).map_err(truncated)?;
    if &head[..8] != MAGIC {
        return Err(bad("not a dfdu snapshot"));
    }
    let version = u32::from_le_bytes(head[8..].try_into().expect("4 bytes"));
    if !matches!(version, VERSION | VERSION_1 | VERSION_2) {
        return Err(bad(format!(
            "snapshot format {version}; this dfdu reads {VERSION}"
        )));
    }
    let fr = FrameReader {
        r,
        cur: vec![],
        pos: 0,
        frames: 0,
        crcs: vec![],
        footer_entries: None,
    };
    let mut rec = Rec(BufReader::with_capacity(1 << 16, fr));
    let root_path = rec.bytes(limits.max_name.max(1 << 16))?;
    if root_path.is_empty() {
        return Err(bad("no root path"));
    }
    let header = Header {
        tool_version: String::from_utf8_lossy(&rec.bytes(256)?).into_owned(),
        written: rec.u()?,
        cross_mounts: false,
        network: false,
        journal: None,
    };
    let opts = rec.u()?;
    let journal = if version != VERSION_1 && opts & OPT_JOURNAL != 0 {
        let volume = rec.bytes(16)?;
        Some(Journal {
            volume: volume.try_into().map_err(|_| bad("bad journal identity"))?,
            event_id: rec.u()?,
            time: if version == VERSION { rec.u()? } else { 0 },
        })
    } else {
        None
    };
    let header = Header {
        cross_mounts: opts & 1 != 0,
        network: opts & 2 != 0,
        journal,
        ..header
    };

    let mut tree = Tree::new();
    let mut devs: Vec<u64> = vec![];
    let mut links: HashSet<(u64, u64)> = HashSet::new();
    let mut stack: Vec<Ix> = vec![];
    let mut root = None;
    let mut entries = 0u64;
    loop {
        let tag = rec.byte()?;
        if tag == CLOSE {
            let Some(d) = stack.pop() else {
                return Err(bad("a close record with nothing open"));
            };
            if let Some(dr) = tree.dir_mut(d) {
                dr.outcome = dr.state;
                dr.pending = 0;
                dr.listing_closed = true;
                dr.reported = true;
            }
            if stack.is_empty() {
                break;
            }
            continue;
        }
        if tag != DIR && tag != ENTRY {
            return Err(bad(format!("unknown record {tag}")));
        }
        entries += 1;
        if entries > limits.max_entries {
            return Err(ImportError::Limit(format!(
                "more than {} entries",
                limits.max_entries
            )));
        }
        let name = rec.bytes(limits.max_name)?;
        let (kind, link) = if tag == ENTRY {
            let k = match rec.byte()? {
                0 => Kind::File,
                2 => Kind::Symlink,
                3 => Kind::Other,
                _ => return Err(bad("a bad entry kind")),
            };
            (k, rec.byte()? != 0)
        } else {
            (Kind::Dir, false)
        };
        let apparent = rec.u()?;
        let alloc = rec.u()?;
        let mtime = rec.i()?;
        let di = rec.u()? as usize;
        let dev = match di.cmp(&devs.len()) {
            std::cmp::Ordering::Less => devs[di],
            std::cmp::Ordering::Equal => {
                let d = rec.u()?;
                devs.push(d);
                d
            }
            std::cmp::Ordering::Greater => return Err(bad("a bad device index")),
        };
        let ino = rec.u()?;
        let err = match rec.byte()? {
            0 => None,
            1 => Some(ErrKind::PermissionDenied),
            2 => Some(ErrKind::NotFound),
            3 => Some(ErrKind::Stalled),
            4 => Some(ErrKind::Other),
            5 => Some(ErrKind::Os(
                i32::try_from(rec.i()?).map_err(|_| bad("a bad error code"))?,
            )),
            _ => return Err(bad("a bad error kind")),
        };
        let meta = Meta {
            kind,
            apparent,
            alloc,
            mtime,
            dev,
            ino,
            nlink: if link { 2 } else { 1 },
            mount: 0,
        };
        let parent = stack.last().copied();
        if root.is_none() && (tag != DIR || !name.is_empty()) {
            return Err(bad("the first record is not the root"));
        }
        if root.is_some() {
            if parent.is_none() {
                return Err(bad("records after the root closed"));
            }
            if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
                return Err(bad(format!("bad name \"{}\"", name.escape_ascii())));
            }
        }
        if tag == DIR {
            let all = DIR_FLAGS.iter().fold(0, |a, &f| a | f);
            let flags = rec.u()? as u16 & all;
            let state = match rec.byte()? {
                0 => DirState::Pending,
                1 => DirState::Complete,
                2 => DirState::Partial,
                3 => DirState::Failed,
                4 => DirState::Skipped,
                5 => DirState::Cancelled,
                6 => DirState::Abandoned,
                7 => DirState::Lost,
                8 => DirState::Superseded,
                9 => DirState::Deleting,
                10 => DirState::Deleted,
                _ => return Err(bad("a bad directory state")),
            };
            // A directory still being read when the snapshot was taken is shown as partial.
            let state = match state {
                DirState::Pending | DirState::Deleting | DirState::Superseded => DirState::Partial,
                s => s,
            };
            let ix = match parent {
                None => tree.new_root(&root_path, &meta, true),
                Some(p) => {
                    if stack.len() > limits.max_depth {
                        return Err(ImportError::Limit(format!(
                            "directories nested more than {} deep",
                            limits.max_depth
                        )));
                    }
                    let ix = tree.add_dir_flagged(p, &name, &meta, flags);
                    let own = tree.own(ix);
                    tree.propagate_add(p, &own);
                    ix
                }
            };
            if parent.is_none() && flags != 0 {
                for f in [NodeFlags::KERNEL, NodeFlags::NETWORK, NodeFlags::DUP_MOUNT] {
                    if flags & f != 0 {
                        tree.set_flag(ix, f, true);
                    }
                }
            }
            if let Some(e) = err {
                tree.set_error(ix, e);
            }
            if let Some(d) = tree.dir_mut(ix) {
                d.state = state;
            }
            root.get_or_insert(ix);
            stack.push(ix);
        } else {
            let p = parent.expect("checked above");
            let charged = !meta.is_hardlink() || links.insert((dev, ino));
            let ix = tree.add_file(p, &name, &meta, charged, err);
            let own = tree.own(ix);
            tree.propagate_add(p, &own);
        }
    }
    // The records end where the frames do, and the footer agrees.
    let mut rest = [0u8; 1];
    let more = rec.0.read(&mut rest).map_err(|e| bad(e.to_string()))?;
    if more != 0 {
        return Err(bad("records after the root closed"));
    }
    let counted = rec.0.get_ref().footer_entries;
    if counted != Some(entries) {
        return Err(bad("the footer does not match the records"));
    }
    Ok(Loaded {
        tree,
        root: root.expect("a root"),
        header,
        entries,
    })
}

#[cfg(test)]
mod tests;
