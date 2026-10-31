//! ncdu's JSON export format, version 1.2 (<https://dev.yorhel.nl/ncdu/jsonfmt>): a top-level
//! array `[1, 2, metadata, root]`, each directory an array of its own info block followed by
//! its entries, files as info blocks. Both directions stream: the export walks the tree with a
//! stack of its own, the import builds the tree as it reads and keeps nothing else but the
//! hard links seen. Input compressed with zstd (ncdu `--compress`) is recognised by its magic
//! number.
//!
//! What maps to what:
//! - `dsize` is the entry's own allocation and `asize` its length; directory totals are
//!   recomputed on import.
//! - Hard links (`nlink` above 1, or `hlnkc`) count once per `(dev, ino)`, as in a scan; the
//!   export writes for `nlink` the number of links in the tree (the tree does not keep the
//!   link count), at least 2.
//! - Directories not entered are written as info blocks with `excluded`: `otherfs` for other
//!   filesystems, `kernfs` for kernel ones, `pattern` for the rest. On import they are
//!   directories whose state is `Skipped`; `frmlink` entries are symlinks.
//! - `read_error` marks entries with an error and directories not read to the end.
//! - Names are bytes. ncdu writes them as they are, so a name that is not UTF-8 makes the
//!   export invalid JSON for most readers; the export counts them.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};

pub mod binary;
mod write;

pub use write::{ExportStats, export};

use dfdu_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, Tree};

use crate::json::{Num, Reader};
use crate::{ImportError, Limits};

/// The metadata block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub progname: Option<String>,
    pub progver: Option<String>,
    pub timestamp: Option<u64>,
}

pub struct Imported {
    /// The tree, with one visible root named by the export's root path.
    pub tree: Tree,
    pub root: Ix,
    pub minor: u64,
    pub meta: Metadata,
    pub entries: u64,
}

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Reads an export, plain or zstd-compressed.
///
/// # Errors
///
/// When `r` fails, the export is malformed or not one this version reads, or it exceeds
/// `limits`.
pub fn import(r: impl Read, limits: &Limits) -> Result<Imported, ImportError> {
    let mut r = BufReader::with_capacity(1 << 16, r);
    let compressed = r.fill_buf()?.starts_with(&ZSTD_MAGIC);
    if compressed {
        let d = crate::ZstdReader::new(r);
        import_plain(BufReader::with_capacity(1 << 16, d), limits)
    } else {
        import_plain(r, limits)
    }
}

/// An info block, with ncdu's defaults for what it leaves out.
#[derive(Default)]
struct Info {
    name: Vec<u8>,
    asize: u64,
    dsize: u64,
    dev: Option<u64>,
    ino: u64,
    hlnkc: bool,
    nlink: u64,
    read_error: bool,
    excluded: Option<Vec<u8>>,
    notreg: bool,
    mode: Option<u64>,
    mtime: u64,
}

fn read_info<R: BufRead>(r: &mut Reader<R>, limits: &Limits) -> Result<Info, ImportError> {
    let mut info = Info::default();
    let mut named = false;
    r.expect(b'{')?;
    if r.peek()? == Some(b'}') {
        r.expect(b'}')?;
    } else {
        loop {
            let key = r.string(256)?;
            r.expect(b':')?;
            let num = |r: &mut Reader<R>| -> Result<u64, ImportError> {
                r.number()?
                    .as_u64()
                    .ok_or_else(|| r.error(format!("{} out of range", key.escape_ascii())))
            };
            match key.as_slice() {
                b"name" => {
                    info.name = r.string(limits.max_name)?;
                    named = true;
                }
                b"asize" => info.asize = num(r)?,
                b"dsize" => info.dsize = num(r)?,
                b"dev" => info.dev = Some(num(r)?),
                b"ino" => info.ino = num(r)?,
                b"nlink" => info.nlink = num(r)?,
                b"mode" => info.mode = Some(num(r)?),
                b"mtime" => info.mtime = num(r)?,
                b"hlnkc" => info.hlnkc = r.literal()? == Some(true),
                b"read_error" => info.read_error = r.literal()? == Some(true),
                b"notreg" => info.notreg = r.literal()? == Some(true),
                b"excluded" => info.excluded = Some(r.string(256)?),
                _ => r.skip(16)?,
            }
            if !r.more(b'}')? {
                break;
            }
        }
    }
    if !named {
        return Err(r.error("an entry without a name"));
    }
    Ok(info)
}

/// The metadata block: the fields known here, others skipped.
fn read_metadata<R: BufRead>(r: &mut Reader<R>) -> Result<Metadata, ImportError> {
    let mut m = Metadata::default();
    r.expect(b'{')?;
    if r.peek()? == Some(b'}') {
        r.expect(b'}')?;
        return Ok(m);
    }
    loop {
        let key = r.string(256)?;
        r.expect(b':')?;
        let text = |r: &mut Reader<R>| -> Result<Option<String>, ImportError> {
            if r.peek()? == Some(b'"') {
                Ok(Some(String::from_utf8_lossy(&r.string(256)?).into_owned()))
            } else {
                r.skip(16).map(|()| None)
            }
        };
        match key.as_slice() {
            b"progname" => m.progname = text(r)?,
            b"progver" => m.progver = text(r)?,
            b"timestamp" if matches!(r.peek()?, Some(b'0'..=b'9')) => {
                m.timestamp = r.number()?.as_u64();
            }
            _ => r.skip(16)?,
        }
        if !r.more(b'}')? {
            return Ok(m);
        }
    }
}

fn valid_name(n: &[u8]) -> bool {
    !n.is_empty() && n != b"." && n != b".." && !n.contains(&b'/') && !n.contains(&0)
}

/// The kind an info block describes, from `mode` where the export has it.
fn kind_of(info: &Info) -> Kind {
    match info.mode.map(|m| m & 0o170_000) {
        Some(0o040_000) => Kind::Dir,
        Some(0o100_000) => Kind::File,
        Some(0o120_000) => Kind::Symlink,
        Some(_) => Kind::Other,
        None if info.notreg => Kind::Other,
        None => Kind::File,
    }
}

fn meta_of(info: &Info, kind: Kind, dev: u64) -> Meta {
    let link = kind != Kind::Dir && (info.nlink > 1 || info.hlnkc);
    Meta {
        kind,
        apparent: info.asize,
        alloc: info.dsize,
        mtime: i64::try_from(info.mtime).unwrap_or(i64::MAX),
        dev,
        ino: info.ino,
        nlink: if link {
            u32::try_from(info.nlink).unwrap_or(u32::MAX).max(2)
        } else {
            1
        },
        mount: 0,
    }
}

/// Marks a directory as read to the end (or not), as the applier would once its listing and
/// every subdirectory were done.
fn close_dir(tree: &mut Tree, ix: Ix) {
    if let Some(d) = tree.dir_mut(ix) {
        if d.state == DirState::Pending {
            d.state = DirState::Complete;
        }
        d.outcome = d.state;
        d.pending = 0;
        d.listing_closed = true;
        d.reported = true;
    }
}

fn set_state(tree: &mut Tree, ix: Ix, state: DirState) {
    if let Some(d) = tree.dir_mut(ix) {
        d.state = state;
    }
}

fn check_entries(entries: u64, limits: &Limits) -> Result<(), ImportError> {
    if entries > limits.max_entries {
        return Err(ImportError::Limit(format!(
            "more than {} entries",
            limits.max_entries
        )));
    }
    Ok(())
}

/// `depth` is the number of open directories, the root included.
fn check_depth(depth: usize, limits: &Limits) -> Result<(), ImportError> {
    if depth > limits.max_depth {
        return Err(ImportError::Limit(format!(
            "directories nested more than {} deep",
            limits.max_depth
        )));
    }
    Ok(())
}

/// Adds the export's root directory; returns it and its device.
fn add_root(tree: &mut Tree, info: &Info) -> (Ix, u64) {
    let dev = info.dev.unwrap_or(0);
    let root = tree.new_root(&info.name, &meta_of(info, Kind::Dir, dev), true);
    if info.read_error {
        tree.set_error(root, ErrKind::Other);
        set_state(tree, root, DirState::Partial);
    }
    (root, dev)
}

/// Adds one entry below `dir`, whose device is `pdev`. `listed`: a directory whose entries
/// follow, left open for them (the caller closes it); otherwise the entry is complete.
fn add_entry(
    tree: &mut Tree,
    links: &mut HashSet<(u64, u64)>,
    dir: Ix,
    pdev: u64,
    info: &Info,
    listed: bool,
) -> Ix {
    let dev = info.dev.unwrap_or(pdev);
    if listed {
        let flags = if dev == pdev {
            NodeFlags::default()
        } else {
            NodeFlags::MOUNT_ROOT
        };
        let ix = tree.add_dir_flagged(dir, &info.name, &meta_of(info, Kind::Dir, dev), flags);
        let own = tree.own(ix);
        tree.propagate_add(dir, &own);
        if info.read_error {
            tree.set_error(ix, ErrKind::Other);
            set_state(tree, ix, DirState::Partial);
        }
        return ix;
    }
    let ix = if let Some(why) = &info.excluded {
        let (kind, flags) = match why.as_slice() {
            b"otherfs" | b"othfs" => (Kind::Dir, NodeFlags::MOUNT_ROOT),
            b"kernfs" => (Kind::Dir, NodeFlags::KERNEL),
            b"frmlink" => (Kind::Symlink, NodeFlags::default()),
            // Not stat'ed: a directory as far as anyone knows, unless `mode` says
            // otherwise.
            _ => match info.mode {
                Some(_) => (kind_of(info), NodeFlags::default()),
                None => (Kind::Dir, NodeFlags::default()),
            },
        };
        let m = meta_of(info, kind, dev);
        if kind == Kind::Dir {
            let ix = tree.add_dir_flagged(dir, &info.name, &m, flags);
            set_state(tree, ix, DirState::Skipped);
            close_dir(tree, ix);
            ix
        } else {
            tree.add_file(dir, &info.name, &m, true, None)
        }
    } else {
        let kind = kind_of(info);
        let m = meta_of(info, kind, dev);
        if kind == Kind::Dir {
            // A directory written as an info block: one that could not be read.
            let ix = tree.add_dir(dir, &info.name, &m);
            set_state(tree, ix, DirState::Failed);
            close_dir(tree, ix);
            ix
        } else {
            let charged = !m.is_hardlink() || links.insert((dev, info.ino));
            let err = info.read_error.then_some(ErrKind::Other);
            tree.add_file(dir, &info.name, &m, charged, err)
        }
    };
    let own = tree.own(ix);
    tree.propagate_add(dir, &own);
    if info.read_error && tree.dir(ix).is_some() {
        tree.set_error(ix, ErrKind::Other);
    }
    ix
}

fn import_plain<R: BufRead>(r: R, limits: &Limits) -> Result<Imported, ImportError> {
    let mut r = Reader::new(r);
    r.expect(b'[')?;
    match r.number()? {
        Num::U(1) => {}
        n => return Err(r.error(format!("major version {n:?}, not 1"))),
    }
    r.expect(b',')?;
    let minor = r
        .number()?
        .as_u64()
        .ok_or_else(|| r.error("bad minor version"))?;
    r.expect(b',')?;
    let meta = read_metadata(&mut r)?;
    r.expect(b',')?;

    let mut tree = Tree::new();
    let mut links: HashSet<(u64, u64)> = HashSet::new();
    r.expect(b'[')?;
    let info = read_info(&mut r, limits)?;
    if info.name.is_empty() || info.name.contains(&0) {
        return Err(r.error("bad root name"));
    }
    let (root, dev) = add_root(&mut tree, &info);
    let mut entries = 1u64;
    let mut stack: Vec<(Ix, u64)> = vec![(root, dev)];
    while let Some(&(dir, pdev)) = stack.last() {
        if !r.more(b']')? {
            close_dir(&mut tree, dir);
            stack.pop();
            continue;
        }
        entries += 1;
        check_entries(entries, limits)?;
        let nested = match r.peek()? {
            Some(b'[') => {
                r.expect(b'[')?;
                true
            }
            Some(b'{') => false,
            _ => return Err(r.error("expected an entry")),
        };
        let info = read_info(&mut r, limits)?;
        if !valid_name(&info.name) {
            return Err(r.error(format!("bad name \"{}\"", info.name.escape_ascii())));
        }
        if nested {
            check_depth(stack.len(), limits)?;
        }
        let ix = add_entry(&mut tree, &mut links, dir, pdev, &info, nested);
        if nested {
            stack.push((ix, info.dev.unwrap_or(pdev)));
        }
    }
    r.expect(b']')?;
    if r.peek()?.is_some() {
        return Err(r.error("data after the export"));
    }
    Ok(Imported {
        tree,
        root,
        minor,
        meta,
        entries,
    })
}

#[cfg(test)]
pub(crate) mod tests;
