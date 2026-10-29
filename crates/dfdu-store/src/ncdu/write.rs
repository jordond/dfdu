//! Writing an export: the tree walked with a stack of its own, one info block per entry.

use std::collections::HashMap;
use std::io::{self, Write};

use dfdu_tree::{DirState, Ix, Kind, NodeFlags, Tree};

use super::Metadata;
use crate::json::write_str;

/// What [`export`] wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportStats {
    pub entries: u64,
    /// Names that are not UTF-8, written as they are.
    pub non_utf8: u64,
}

/// Writes the tree below `root` (its path as the root's name) in ncdu's format, version 1.2.
///
/// # Errors
///
/// When writing to `w` fails.
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
        if let Some(&c) = kids.get(*i) {
            *i += 1;
            w.write_all(b",\n")?;
            next = Some((c, Some(*dev)));
        } else {
            stack.pop();
            w.write_all(b"]")?;
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
mod tests;
