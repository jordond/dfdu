//! Writing a snapshot: the tree's records, in checked frames.

use std::io::{self, Write};

use dfdu_tree::{DirState, ErrKind, Ix, Kind, NodeFlags, Tree};

use super::{CLOSE, DIR, DIR_FLAGS, ENTRY, FRAME_RAW, Header, MAGIC, OPT_JOURNAL, VERSION};

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
        let too_big = |_| io::Error::new(io::ErrorKind::InvalidInput, "a frame over 4 GiB");
        let zlen = u32::try_from(z.len()).map_err(too_big)?;
        let rawlen = u32::try_from(self.raw.len()).map_err(too_big)?;
        let mut h = [0u8; 12];
        h[..4].copy_from_slice(&zlen.to_le_bytes());
        h[4..8].copy_from_slice(&rawlen.to_le_bytes());
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

/// [`write`] in format `version` (2 or 3; tests write older formats with it).
pub(super) fn write_as(
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
    put_header(&mut fw.raw, &path, header, version);
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
                r.push(u8::from(flags.has(NodeFlags::HARDLINK)));
            }
            let (apparent, alloc) = tree.raw_size(ix);
            put_u(r, apparent);
            put_u(r, alloc);
            put_i(r, tree.mtime(ix));
            let (dev, ino) = tree.dev_ino(ix);
            if let Some(i) = devs.iter().position(|&d| d == dev) {
                put_u(r, i as u64);
            } else {
                put_u(r, devs.len() as u64);
                put_u(r, dev);
                devs.push(dev);
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
        if let Some(&c) = kids.get(*i) {
            *i += 1;
            next = Some(c);
        } else {
            stack.pop();
            fw.raw.push(CLOSE);
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
        if version == VERSION {
            put_u(r, j.time);
        }
    }
}

#[cfg(test)]
mod tests;
