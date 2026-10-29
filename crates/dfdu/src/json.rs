//! JSON output of `scan`, `report` and `clean`. Every
//! document has a top-level `schema_version`; sizes are integers in bytes; paths and names are
//! UTF-8 strings, with the raw bytes in base64 beside them (`path_bytes_b64`, `name_bytes_b64`)
//! when they are not valid UTF-8. The schemas are in `schema/` at the top of the repository.

use std::io::{self, Write};
use std::path::Path;

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::{Class, DirState, HostPath, Ix, Kind, NodeFlags, Totals, Tree};
use serde_json::{Map, Value, json};

/// Bumped when a field changes meaning or goes away; new fields keep the version.
pub const SCHEMA_VERSION: u64 = 1;

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() {
                ABC[(n >> shift & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Adds `key` with the text of `bytes`, and `key_bytes_b64` when they are not UTF-8.
fn put_bytes(o: &mut Map<String, Value>, key: &str, bytes: &[u8]) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        o.insert(key.into(), s.into());
    } else {
        o.insert(key.into(), String::from_utf8_lossy(bytes).into());
        o.insert(format!("{key}_bytes_b64"), base64(bytes).into());
    }
}

/// A path on this machine.
pub fn put_path(o: &mut Map<String, Value>, key: &str, path: &Path) {
    put_bytes(o, key, path.as_os_str().as_encoded_bytes());
}

/// A path of a tree, on the host it was scanned on.
pub fn put_host_path(o: &mut Map<String, Value>, key: &str, path: &HostPath) {
    put_bytes(o, key, path.as_bytes());
}

pub fn tool() -> Value {
    json!({ "name": "dfdu", "version": env!("CARGO_PKG_VERSION") })
}

/// The fields every document starts with: `schema_version`, `tool` and `root`.
pub fn document(root: &HostPath) -> Map<String, Value> {
    let mut doc = Map::new();
    doc.insert("schema_version".into(), SCHEMA_VERSION.into());
    doc.insert("tool".into(), tool());
    put_host_path(&mut doc, "root", root);
    doc
}

/// Writes `doc` to stdout, pretty-printed, with a final newline.
pub fn print(doc: Map<String, Value>) -> io::Result<()> {
    let mut w = io::stdout().lock();
    serde_json::to_writer_pretty(&mut w, &Value::Object(doc))?;
    w.write_all(b"\n")
}

pub fn mode_name(mode: SizeMode) -> &'static str {
    match mode {
        SizeMode::Disk => "disk",
        SizeMode::Apparent => "apparent",
        SizeMode::Referenced => "du",
    }
}

pub fn totals(t: &Totals) -> Value {
    json!({
        "disk": t.alloc,
        "apparent": t.apparent,
        "du": t.referenced,
        "shared_outside": t.shared_out,
        "entries": t.items,
        "errors": t.errors,
    })
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::File => "file",
        Kind::Dir => "dir",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

/// A rule match: the rule, its category and the safety level that holds for this node.
pub fn class(m: &Model, c: Class) -> Value {
    let mut o = Map::new();
    if let Some(rules) = &m.rules {
        let r = rules.rule(c.rule);
        o.insert("rule".into(), r.id.clone().into());
        o.insert("category".into(), r.category().id.into());
    }
    o.insert("safety".into(), c.safety.as_str().into());
    Value::Object(o)
}

/// One entry, without its children.
fn entry(m: &Model, ix: Ix, note: &str) -> Map<String, Value> {
    let t = &m.tree;
    let tot = t.total(ix);
    let mut o = Map::new();
    put_bytes(&mut o, "name", t.name(ix));
    o.insert("type".into(), kind_name(t.kind(ix)).into());
    o.insert("disk".into(), tot.alloc.into());
    o.insert("apparent".into(), tot.apparent.into());
    o.insert("du".into(), tot.referenced.into());
    o.insert("entries".into(), tot.items.into());
    o.insert("errors".into(), tot.errors.into());
    o.insert("mtime".into(), t.mtime(ix).into());
    if let Some(c) = t.class(ix) {
        o.insert("class".into(), class(m, c));
    }
    if !note.is_empty() {
        o.insert("note".into(), note.into());
    }
    o
}

/// Children of `ix`, largest first in `mode`.
fn sorted_children(tree: &Tree, ix: Ix, mode: SizeMode) -> Vec<Ix> {
    let mut kids: Vec<Ix> = tree.children(ix).collect();
    kids.sort_by_key(|&c| std::cmp::Reverse(mode.of(&tree.total(c))));
    kids
}

/// The tree below `ix` to `depth` levels, `top` children per directory; the others are summed
/// in `rest`.
fn subtree(
    m: &Model,
    ix: Ix,
    depth: usize,
    top: usize,
    mode: SizeMode,
    note: &dyn Fn(Ix) -> String,
) -> Value {
    let mut o = entry(m, ix, &note(ix));
    if depth > 0 && m.tree.dir(ix).is_some() {
        let kids = sorted_children(&m.tree, ix, mode);
        let shown: Vec<Value> = kids
            .iter()
            .take(top)
            .map(|&c| subtree(m, c, depth - 1, top, mode, note))
            .collect();
        o.insert("children".into(), shown.into());
        if kids.len() > top {
            let mut rest = Totals::default();
            for &c in &kids[top..] {
                rest.add(&m.tree.total(c));
            }
            o.insert(
                "rest".into(),
                json!({
                    "count": kids.len() - top,
                    "disk": rest.alloc,
                    "apparent": rest.apparent,
                    "du": rest.referenced,
                    "entries": rest.items,
                }),
            );
        }
        if m.tree.shared_here(ix) > 0 {
            o.insert(
                "shared_between_entries".into(),
                m.tree.shared_here(ix).into(),
            );
        }
    }
    Value::Object(o)
}

/// Every entry that could not be read.
fn errors(m: &Model, root: Ix) -> Vec<Map<String, Value>> {
    let list = dfdu_engine::errors(&m.tree, root, usize::MAX);
    list.items
        .iter()
        .map(|e| {
            let mut o = Map::new();
            put_host_path(&mut o, "path", &e.path);
            o.insert("dir".into(), e.is_dir.into());
            o.insert("problem".into(), e.problem.clone().into());
            o
        })
        .collect()
}

/// Directories not descended into, with the reason.
fn skipped(m: &Model, root: Ix) -> Vec<Map<String, Value>> {
    let t = &m.tree;
    let mut out = vec![];
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        let Some(dir) = t.dir(ix) else { continue };
        let flags = t.flags(ix);
        let reason = if flags.has(NodeFlags::DUP_MOUNT) {
            Some("mounted-again")
        } else if flags.has(NodeFlags::KERNEL) {
            Some("kernel-filesystem")
        } else if flags.has(NodeFlags::NETWORK) {
            Some("network-filesystem")
        } else if dir.state == DirState::Skipped {
            Some("other-filesystem")
        } else {
            None
        };
        match reason {
            Some(reason) => {
                let mut item = Map::new();
                put_host_path(&mut item, "path", &t.host_path(ix));
                item.insert("reason".into(), reason.into());
                out.push(item);
            }
            None => stack.extend(t.children(ix)),
        }
    }
    out.reverse();
    out
}

pub struct ScanOut<'a> {
    pub mode: SizeMode,
    pub depth: Option<usize>,
    pub top: Option<usize>,
    pub elapsed_ms: u64,
    pub note: &'a dyn Fn(Ix) -> String,
    /// What the scan did not enter, for `not_entered` and `links_outside`: fields of the JSON
    /// document, and of the NDJSON summary record.
    pub outside: Option<&'a crate::outside::Outside>,
}

/// `scan --format json`: one document.
pub fn scan(w: &mut dyn Write, m: &Model, root: Ix, o: &ScanOut) -> io::Result<()> {
    let mut doc = document(&m.tree.host_path(root));
    doc.insert("size_mode".into(), mode_name(o.mode).into());
    doc.insert("complete".into(), m.complete(root).into());
    doc.insert("elapsed_ms".into(), o.elapsed_ms.into());
    doc.insert("totals".into(), totals(&m.tree.total(root)));
    let tree = subtree(
        m,
        root,
        o.depth.unwrap_or(1),
        o.top.unwrap_or(10),
        o.mode,
        o.note,
    );
    doc.insert("tree".into(), tree);
    doc.insert("errors".into(), errors(m, root).into());
    doc.insert("skipped".into(), skipped(m, root).into());
    if let Some(left_out) = o.outside {
        crate::outside::put_json(&mut doc, left_out, m.rules.as_deref());
    }
    serde_json::to_writer_pretty(&mut *w, &Value::Object(doc))?;
    writeln!(w)
}

fn line(w: &mut dyn Write, record: &str, mut o: Map<String, Value>) -> io::Result<()> {
    o.insert("record".into(), record.into());
    serde_json::to_writer(&mut *w, &Value::Object(o))?;
    writeln!(w)
}

/// `scan --format ndjson`: a `header` line, one `entry` line per entry (depth first, largest
/// first, every level unless `depth` is given), then `error` and `skipped` lines and a
/// `summary` line.
pub fn scan_ndjson(w: &mut dyn Write, m: &Model, root: Ix, o: &ScanOut) -> io::Result<()> {
    let t = &m.tree;
    let mut header = document(&t.host_path(root));
    header.insert("size_mode".into(), mode_name(o.mode).into());
    line(w, "header", header)?;
    let max_depth = o.depth.unwrap_or(usize::MAX);
    let top = o.top.unwrap_or(usize::MAX);
    let mut stack = vec![(root, 0usize)];
    while let Some((ix, depth)) = stack.pop() {
        let mut record = entry(m, ix, &(o.note)(ix));
        record.remove("name");
        record.remove("name_bytes_b64");
        put_host_path(&mut record, "path", &t.host_path(ix));
        record.insert("depth".into(), depth.into());
        line(w, "entry", record)?;
        if depth < max_depth && t.dir(ix).is_some() {
            for &child in sorted_children(t, ix, o.mode).iter().take(top).rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    for error in errors(m, root) {
        line(w, "error", error)?;
    }
    for skip in skipped(m, root) {
        line(w, "skipped", skip)?;
    }
    let mut summary = Map::new();
    summary.insert("complete".into(), m.complete(root).into());
    summary.insert("elapsed_ms".into(), o.elapsed_ms.into());
    summary.insert("totals".into(), totals(&t.total(root)));
    if let Some(left_out) = o.outside {
        crate::outside::put_json(&mut summary, left_out, m.rules.as_deref());
    }
    line(w, "summary", summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x80]), "//6A");
    }
}
