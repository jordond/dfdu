//! Reading a snapshot back, every frame and the footer checked ([`frames`]).

mod frames;

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};

use dfdu_scan::journal::JournalPosition;
use dfdu_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, Tree};

use super::{
    CLOSE, DIR, DIR_FLAGS, ENTRY, Header, Loaded, MAGIC, OPT_JOURNAL, VERSION, VERSION_1,
    VERSION_2, VERSION_3,
};
use crate::ImportError;
use crate::Limits;
use frames::{FrameReader, Rec, bad, bad_at, frame_error, le_u32, truncated};

/// Whether `bytes` start like a snapshot.
#[must_use]
pub fn is_snapshot(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Reads a snapshot. Anything damaged, cut short or past `limits` is an error, and nothing of
/// it is used.
///
/// # Errors
///
/// When `r` fails, the data is not a snapshot of a format this version reads, it is damaged
/// or cut short, or it exceeds `limits`.
pub fn read(r: impl Read, limits: &Limits) -> Result<Loaded, ImportError> {
    let (mut rec, version) = open_records(r)?;
    let (root_path, header) = read_header_records(&mut rec, version, limits)?;

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
        let (name, meta, err) = read_entry(&mut rec, tag, limits, &mut devs)?;
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
            let (flags, state) = read_dir_fields(&mut rec)?;
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
            if parent.is_none() && flags != NodeFlags::default() {
                for f in [NodeFlags::KERNEL, NodeFlags::NETWORK, NodeFlags::DUP_MOUNT] {
                    if flags.has(f) {
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
            // An entry comes after the root (checked above), so inside an open directory.
            let Some(p) = parent else {
                return Err(bad("records after the root closed"));
            };
            let charged = !meta.is_hardlink() || links.insert((meta.dev, meta.ino));
            let ix = tree.add_file(p, &name, &meta, charged, err);
            let own = tree.own(ix);
            tree.propagate_add(p, &own);
        }
    }
    check_end(rec, entries)?;
    // The loop ends only when the root closes, so there is one.
    let root = root.ok_or_else(|| bad("no root"))?;
    Ok(Loaded {
        tree,
        root,
        header,
        entries,
    })
}

/// Reads only the start of a snapshot: the root's path (raw bytes) and the header. Nothing past
/// the first frame is read or checked.
///
/// # Errors
///
/// When `r` fails, or its start is not that of a snapshot of a format this version reads.
pub fn read_header(r: impl Read, limits: &Limits) -> Result<(Vec<u8>, Header), ImportError> {
    let (mut rec, version) = open_records(r)?;
    read_header_records(&mut rec, version, limits)
}

/// The records of a snapshot read from `R`.
type Records<R> = Rec<BufReader<FrameReader<BufReader<R>>>>;

/// Checks the magic and the format version, and returns the record stream and the version.
fn open_records<R: Read>(r: R) -> Result<(Records<R>, u32), ImportError> {
    let mut r = BufReader::with_capacity(1 << 16, r);
    let mut head = [0u8; 12];
    r.read_exact(&mut head).map_err(truncated)?;
    if &head[..8] != MAGIC {
        return Err(bad_at(0, "not a dfdu snapshot"));
    }
    let version = le_u32(&head, 8);
    if !matches!(version, VERSION | VERSION_1 | VERSION_2 | VERSION_3) {
        return Err(bad_at(
            8,
            format!("snapshot format {version}; this dfdu reads {VERSION}"),
        ));
    }
    let fr = FrameReader::new(r);
    Ok((Rec(BufReader::with_capacity(1 << 16, fr)), version))
}

/// Checks that the records end where the frames do, and that the footer counts `entries`.
fn check_end<R: Read>(
    mut rec: Rec<BufReader<FrameReader<R>>>,
    entries: u64,
) -> Result<(), ImportError> {
    let mut rest = [0u8; 1];
    let more = rec.0.read(&mut rest).map_err(frame_error)?;
    if more != 0 {
        return Err(bad("records after the root closed"));
    }
    if rec.0.get_ref().footer_entries != Some(entries) {
        return Err(bad("the footer does not match the records"));
    }
    Ok(())
}

/// The scan's header records ([`put_header`](super::write)): the root path and the header.
fn read_header_records(
    rec: &mut Rec<impl BufRead>,
    version: u32,
    limits: &Limits,
) -> Result<(Vec<u8>, Header), ImportError> {
    let root_path = rec.bytes(limits.max_name.max(1 << 16))?;
    if root_path.is_empty() {
        return Err(bad("no root path"));
    }
    let tool_version = String::from_utf8_lossy(&rec.bytes(256)?).into_owned();
    let written = rec.u()?;
    let opts = rec.u()?;
    let journal = if version != VERSION_1 && opts & OPT_JOURNAL != 0 {
        let volume = rec.bytes(16)?;
        Some(JournalPosition {
            volume: volume.try_into().map_err(|_| bad("bad journal identity"))?,
            event_id: rec.u()?,
            time: if version >= VERSION_3 { rec.u()? } else { 0 },
        })
    } else {
        None
    };
    let (last_full_scan, scan_ms) = if version >= VERSION {
        (rec.u()?, rec.u()?)
    } else {
        (0, 0)
    };
    let header = Header {
        tool_version,
        written,
        cross_mounts: opts & 1 != 0,
        network: opts & 2 != 0,
        journal,
        last_full_scan,
        scan_ms,
    };
    Ok((root_path, header))
}

/// The fields every record `tag` (`DIR` or `ENTRY`) has: its name, metadata and error. A new
/// device goes into `devs`.
fn read_entry(
    rec: &mut Rec<impl BufRead>,
    tag: u8,
    limits: &Limits,
    devs: &mut Vec<u64>,
) -> Result<(Vec<u8>, Meta, Option<ErrKind>), ImportError> {
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
    // An index that does not fit in `usize` is past the table too.
    let di = usize::try_from(rec.u()?).unwrap_or(usize::MAX);
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
    Ok((name, meta, err))
}

/// The fields only a directory record has: its kept flags and its state.
fn read_dir_fields(rec: &mut Rec<impl BufRead>) -> Result<(NodeFlags, DirState), ImportError> {
    // Only the bits of DIR_FLAGS are kept.
    let bits = rec.u()?;
    let flags = DIR_FLAGS
        .iter()
        .filter(|f| bits & u64::from(f.bits()) != 0)
        .fold(NodeFlags::default(), |a, &f| a | f);
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
    Ok((flags, state))
}

#[cfg(test)]
mod tests;
