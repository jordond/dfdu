//! Writing a snapshot: the tree's records, in checked frames.

use std::io::{self, Write};

use dfdu_tree::{DirState, ErrKind, Ix, Kind, NodeFlags, Tree};

use super::{
    CLOSE, DIR, DIR_FLAGS, ENTRY, FRAME_RAW, Header, MAGIC, OPT_JOURNAL, VERSION, VERSION_3,
};

/// The frames written so far: their count and CRCs, for the footer.
#[derive(Default)]
struct Frames {
    frames: u64,
    crcs: Vec<u8>,
}

impl Frames {
    /// Writes `raw` to `w` as one frame and empties it; nothing when it is empty.
    fn frame(&mut self, w: &mut impl Write, raw: &mut Vec<u8>) -> io::Result<()> {
        if raw.is_empty() {
            return Ok(());
        }
        let z = ruzstd::encoding::compress_to_vec(
            &raw[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let crc = crc32c::crc32c(&z);
        let too_big = |_| io::Error::new(io::ErrorKind::InvalidInput, "a frame over 4 GiB");
        let zlen = u32::try_from(z.len()).map_err(too_big)?;
        let rawlen = u32::try_from(raw.len()).map_err(too_big)?;
        let mut h = [0u8; 12];
        h[..4].copy_from_slice(&zlen.to_le_bytes());
        h[4..8].copy_from_slice(&rawlen.to_le_bytes());
        h[8..].copy_from_slice(&crc.to_le_bytes());
        w.write_all(&h)?;
        w.write_all(&z)?;
        self.crcs.extend_from_slice(&crc.to_le_bytes());
        self.frames += 1;
        raw.clear();
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
    put_u(out, ((v << 1) ^ (v >> 63)).cast_unsigned());
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
        Some(ErrKind::Os(c)) => (5, i64::from(c)),
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
///
/// # Errors
///
/// When writing to `w` fails.
pub fn write(tree: &Tree, root: Ix, w: impl Write, header: &Header) -> io::Result<u64> {
    write_as(tree, root, w, header, VERSION)
}

/// [`write`] in format `version` (2 to 4; tests write older formats with it).
pub(super) fn write_as(
    tree: &Tree,
    root: Ix,
    w: impl Write,
    header: &Header,
    version: u32,
) -> io::Result<u64> {
    let mut w = io::BufWriter::with_capacity(1 << 16, w);
    let mut s = Writer::new_as(tree, root, header, version);
    while !s.step(tree, usize::MAX) {
        s.flush(&mut w)?;
    }
    s.finish(&mut w)
}

/// A snapshot written a part at a time, for a tree that others change between the parts: the
/// caller holds the tree still (a read lock) for each [`Writer::step`], which only adds records
/// in memory, and writes them out with [`Writer::flush`] after letting go of it. The tree must
/// not change between steps (its [`Tree::version`] stays the same); if it does, start again.
pub struct Writer {
    version: u32,
    /// The magic and version are written.
    started: bool,
    raw: Vec<u8>,
    frames: Frames,
    root: Ix,
    devs: Vec<u64>,
    entries: u64,
    stack: Vec<(Vec<Ix>, usize)>,
    next: Option<Ix>,
    done: bool,
}

impl Writer {
    /// A snapshot of the tree below `root` (a visible root, whose name is its path) in the
    /// current format, with `header`.
    #[must_use]
    pub fn new(tree: &Tree, root: Ix, header: &Header) -> Writer {
        Writer::new_as(tree, root, header, VERSION)
    }

    fn new_as(tree: &Tree, root: Ix, header: &Header, version: u32) -> Writer {
        let mut raw = Vec::with_capacity(FRAME_RAW + 4096);
        put_header(&mut raw, &tree.path_buf(root), header, version);
        Writer {
            version,
            started: false,
            raw,
            frames: Frames::default(),
            root,
            devs: vec![],
            entries: 0,
            stack: vec![],
            next: Some(root),
            done: false,
        }
    }

    /// Entries added so far.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.entries
    }

    /// Adds the records of up to `max` more entries of `tree`, stopping early once a frame's
    /// worth waits to be written. True once every entry has been added.
    pub fn step(&mut self, tree: &Tree, max: usize) -> bool {
        let mut added = 0usize;
        while !self.done {
            if let Some(ix) = self.next.take() {
                self.entry(tree, ix);
                added += 1;
                if self.raw.len() >= FRAME_RAW || added >= max {
                    return false;
                }
            }
            let Some((kids, i)) = self.stack.last_mut() else {
                self.done = true;
                break;
            };
            if let Some(&c) = kids.get(*i) {
                *i += 1;
                self.next = Some(c);
            } else {
                self.stack.pop();
                self.raw.push(CLOSE);
            }
        }
        true
    }

    /// The record of entry `ix`; a directory's children are taken to visit next.
    fn entry(&mut self, tree: &Tree, ix: Ix) {
        self.entries += 1;
        let r = &mut self.raw;
        let is_dir = tree.dir(ix).is_some();
        r.push(if is_dir { DIR } else { ENTRY });
        put_bytes(r, if ix == self.root { b"" } else { tree.name(ix) });
        let flags = tree.flags(ix);
        if !is_dir {
            r.push(match tree.kind(ix) {
                Kind::File => 0,
                Kind::Dir => 1,
                Kind::Symlink => 2,
                Kind::Other => 3,
            });
            r.push(u8::from(flags.has(NodeFlags::HARDLINK)));
        }
        let (apparent, alloc) = tree.raw_size(ix);
        put_u(r, apparent);
        put_u(r, alloc);
        put_i(r, tree.mtime(ix));
        let (dev, ino) = tree.dev_ino(ix);
        if let Some(i) = self.devs.iter().position(|&d| d == dev) {
            put_u(r, i as u64);
        } else {
            put_u(r, self.devs.len() as u64);
            put_u(r, dev);
            self.devs.push(dev);
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
            self.stack.push((tree.children(ix).collect(), 0));
        }
    }

    fn start(&mut self, w: &mut impl Write) -> io::Result<()> {
        if !self.started {
            w.write_all(MAGIC)?;
            w.write_all(&self.version.to_le_bytes())?;
            self.started = true;
        }
        Ok(())
    }

    /// Writes the records added so far to `w` when they fill a frame.
    ///
    /// # Errors
    ///
    /// When writing to `w` fails.
    pub fn flush(&mut self, w: &mut impl Write) -> io::Result<()> {
        self.start(w)?;
        if self.raw.len() >= FRAME_RAW {
            self.frames.frame(w, &mut self.raw)?;
        }
        Ok(())
    }

    /// Writes the rest to `w`, once [`Writer::step`] added every entry: the last frame, the
    /// end marker and the footer. Returns the entries written.
    ///
    /// # Errors
    ///
    /// When writing to `w` fails, or entries are left to add.
    pub fn finish(mut self, w: &mut impl Write) -> io::Result<u64> {
        if !self.done {
            return Err(io::Error::other("the snapshot was finished early"));
        }
        self.start(w)?;
        self.frames.frame(w, &mut self.raw)?;
        w.write_all(&[0; 12])?;
        let mut foot = Vec::with_capacity(24);
        foot.extend_from_slice(&self.entries.to_le_bytes());
        foot.extend_from_slice(&self.frames.frames.to_le_bytes());
        foot.extend_from_slice(&crc32c::crc32c(&self.frames.crcs).to_le_bytes());
        let fcrc = crc32c::crc32c(&foot);
        foot.extend_from_slice(&fcrc.to_le_bytes());
        w.write_all(&foot)?;
        w.flush()?;
        Ok(self.entries)
    }
}

/// The scan's header records: root path, tool version, time, options and journal position.
fn put_header(r: &mut Vec<u8>, path: &std::path::Path, header: &Header, version: u32) {
    put_bytes(r, &dfdu_tree::name_bytes(path.as_os_str()));
    put_bytes(r, header.tool_version.as_bytes());
    put_u(r, header.written);
    put_u(
        r,
        u64::from(header.cross_mounts)
            | u64::from(header.network) << 1
            | if header.journal.is_some() {
                OPT_JOURNAL
            } else {
                0
            },
    );
    if let Some(j) = &header.journal {
        put_bytes(r, &j.volume);
        put_u(r, j.event_id);
        if version >= VERSION_3 {
            put_u(r, j.time);
        }
    }
    if version >= VERSION {
        put_u(r, header.last_full_scan);
        put_u(r, header.scan_ms);
    }
}

#[cfg(test)]
mod tests;
