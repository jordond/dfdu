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

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};

use dfdu_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, Tree};

use crate::ImportError;
use crate::json::{Num, Reader, write_str};

/// Limits on what an import accepts, so a hostile or broken file cannot use unbounded memory
/// or stack.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Directory nesting below the root.
    pub max_depth: usize,
    /// Bytes in a name (ncdu's own limit is 32,768).
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
    match info.mode.map(|m| m & 0o170000) {
        Some(0o040000) => Kind::Dir,
        Some(0o100000) => Kind::File,
        Some(0o120000) => Kind::Symlink,
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
    let dev = info.dev.unwrap_or(0);
    let root = tree.new_root(&info.name, &meta_of(&info, Kind::Dir, dev), true);
    if info.read_error {
        tree.set_error(root, ErrKind::Other);
        set_state(&mut tree, root, DirState::Partial);
    }
    let mut entries = 1u64;
    let mut stack: Vec<(Ix, u64)> = vec![(root, dev)];
    while let Some(&(dir, pdev)) = stack.last() {
        if !r.more(b']')? {
            close_dir(&mut tree, dir);
            stack.pop();
            continue;
        }
        entries += 1;
        if entries > limits.max_entries {
            return Err(ImportError::Limit(format!(
                "more than {} entries",
                limits.max_entries
            )));
        }
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
        let dev = info.dev.unwrap_or(pdev);
        if nested {
            if stack.len() > limits.max_depth {
                return Err(ImportError::Limit(format!(
                    "directories nested more than {} deep",
                    limits.max_depth
                )));
            }
            let flags = if dev != pdev {
                NodeFlags::MOUNT_ROOT
            } else {
                0
            };
            let ix = tree.add_dir_flagged(dir, &info.name, &meta_of(&info, Kind::Dir, dev), flags);
            let own = tree.own(ix);
            tree.propagate_add(dir, &own);
            if info.read_error {
                tree.set_error(ix, ErrKind::Other);
                set_state(&mut tree, ix, DirState::Partial);
            }
            stack.push((ix, dev));
            continue;
        }
        let ix = match &info.excluded {
            Some(why) => {
                let (kind, flags) = match why.as_slice() {
                    b"otherfs" | b"othfs" => (Kind::Dir, NodeFlags::MOUNT_ROOT),
                    b"kernfs" => (Kind::Dir, NodeFlags::KERNEL),
                    b"frmlink" => (Kind::Symlink, 0),
                    // Not stat'ed: a directory as far as anyone knows, unless `mode` says
                    // otherwise.
                    _ => match info.mode {
                        Some(_) => (kind_of(&info), 0),
                        None => (Kind::Dir, 0),
                    },
                };
                let m = meta_of(&info, kind, dev);
                if kind == Kind::Dir {
                    let ix = tree.add_dir_flagged(dir, &info.name, &m, flags);
                    set_state(&mut tree, ix, DirState::Skipped);
                    close_dir(&mut tree, ix);
                    ix
                } else {
                    tree.add_file(dir, &info.name, &m, true, None)
                }
            }
            None => {
                let kind = kind_of(&info);
                let m = meta_of(&info, kind, dev);
                if kind == Kind::Dir {
                    // A directory written as an info block: one that could not be read.
                    let ix = tree.add_dir(dir, &info.name, &m);
                    set_state(&mut tree, ix, DirState::Failed);
                    close_dir(&mut tree, ix);
                    ix
                } else {
                    let charged = !m.is_hardlink() || links.insert((dev, info.ino));
                    let err = info.read_error.then_some(ErrKind::Other);
                    tree.add_file(dir, &info.name, &m, charged, err)
                }
            }
        };
        let own = tree.own(ix);
        tree.propagate_add(dir, &own);
        if info.read_error && tree.dir(ix).is_some() {
            tree.set_error(ix, ErrKind::Other);
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

/// What [`export`] wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportStats {
    pub entries: u64,
    /// Names that are not UTF-8, written as they are.
    pub non_utf8: u64,
}

/// Writes the tree below `root` (its path as the root's name) in ncdu's format, version 1.2.
pub fn export(tree: &Tree, root: Ix, w: impl Write, meta: &Metadata) -> io::Result<ExportStats> {
    let mut w = io::BufWriter::with_capacity(1 << 16, w);
    let mut stats = ExportStats::default();
    let links = link_counts(tree, root);
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(b"[1,2,{");
    let mut first = true;
    let mut field = |buf: &mut Vec<u8>, k: &str| {
        if !first {
            buf.push(b',');
        }
        first = false;
        write_str(buf, k.as_bytes());
        buf.push(b':');
    };
    if let Some(p) = &meta.progname {
        field(&mut buf, "progname");
        write_str(&mut buf, p.as_bytes());
    }
    if let Some(v) = &meta.progver {
        field(&mut buf, "progver");
        write_str(&mut buf, v.as_bytes());
    }
    if let Some(t) = meta.timestamp {
        field(&mut buf, "timestamp");
        buf.extend_from_slice(t.to_string().as_bytes());
    }
    buf.extend_from_slice(b"},\n");

    let path = tree.path_buf(root);
    let root_name = dfdu_tree::name_bytes(path.as_os_str()).into_owned();
    // Open directories: their children still to write, and their device.
    let mut stack: Vec<(Vec<Ix>, usize, u64)> = vec![];
    let mut next = Some((root, None::<u64>));
    loop {
        if let Some((ix, parent_dev)) = next.take() {
            let name: &[u8] = if ix == root {
                &root_name
            } else {
                tree.name(ix)
            };
            if std::str::from_utf8(name).is_err() {
                stats.non_utf8 += 1;
            }
            stats.entries += 1;
            let (dev, _) = tree.dev_ino(ix);
            let open = tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped);
            if open {
                buf.push(b'[');
            }
            info_block(&mut buf, tree, ix, name, parent_dev, &links);
            w.write_all(&buf)?;
            buf.clear();
            if open {
                stack.push((tree.children(ix).collect(), 0, dev));
            }
        }
        let Some((kids, i, dev)) = stack.last_mut() else {
            break;
        };
        match kids.get(*i) {
            Some(&c) => {
                *i += 1;
                w.write_all(b",\n")?;
                next = Some((c, Some(*dev)));
            }
            None => {
                stack.pop();
                w.write_all(b"]")?;
            }
        }
    }
    w.write_all(b"]\n")?;
    w.flush()?;
    Ok(stats)
}

/// Links in the tree below `root` per `(dev, ino)`, for `nlink`.
fn link_counts(tree: &Tree, root: Ix) -> HashMap<(u64, u64), u32> {
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        if tree.flags(ix).has(NodeFlags::HARDLINK) {
            *out.entry(tree.dev_ino(ix)).or_insert(0) += 1;
        }
        stack.extend(tree.children(ix));
    }
    out
}

fn info_block(
    buf: &mut Vec<u8>,
    tree: &Tree,
    ix: Ix,
    name: &[u8],
    parent_dev: Option<u64>,
    links: &HashMap<(u64, u64), u32>,
) {
    let num = |buf: &mut Vec<u8>, k: &str, v: u64| {
        if v != 0 {
            buf.extend_from_slice(format!(",\"{k}\":{v}").as_bytes());
        }
    };
    buf.extend_from_slice(b"{\"name\":");
    write_str(buf, name);
    let (asize, dsize) = tree.raw_size(ix);
    let (dev, ino) = tree.dev_ino(ix);
    let flags = tree.flags(ix);
    let dir = tree.dir(ix);
    if let Some(d) = dir
        && d.state == DirState::Skipped
    {
        let why = if flags.has(NodeFlags::KERNEL) {
            "kernfs"
        } else if flags.has(NodeFlags::MOUNT_ROOT)
            || flags.has(NodeFlags::NETWORK)
            || flags.has(NodeFlags::DUP_MOUNT)
        {
            "otherfs"
        } else {
            "pattern"
        };
        buf.extend_from_slice(format!(",\"excluded\":\"{why}\"").as_bytes());
    }
    num(buf, "asize", asize);
    num(buf, "dsize", dsize);
    if parent_dev != Some(dev) {
        buf.extend_from_slice(format!(",\"dev\":{dev}").as_bytes());
    }
    if flags.has(NodeFlags::HARDLINK) {
        let n = links.get(&(dev, ino)).copied().unwrap_or(1).max(2);
        buf.extend_from_slice(format!(",\"ino\":{ino},\"hlnkc\":true,\"nlink\":{n}").as_bytes());
    }
    let unread = dir.is_some_and(|d| !matches!(d.state, DirState::Complete | DirState::Skipped));
    if tree.error(ix).is_some() || unread {
        buf.extend_from_slice(b",\"read_error\":true");
    }
    if matches!(tree.kind(ix), Kind::Symlink | Kind::Other) {
        buf.extend_from_slice(b",\"notreg\":true");
    }
    if let Ok(t) = u64::try_from(tree.mtime(ix)) {
        num(buf, "mtime", t);
    }
    buf.push(b'}');
}

#[cfg(test)]
pub(crate) mod tests;
