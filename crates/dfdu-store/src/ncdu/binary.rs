//! ncdu's binary export format (ncdu 2.6 and later, `ncdu -O`;
//! <https://dev.yorhel.nl/ncdu/binfmt>): a signature, then blocks framed by a type and length
//! at both ends. Data blocks hold zstd-compressed CBOR maps, one per entry; the index block,
//! last, gives each data block's place in the file and the root entry. A directory points at
//! its last entry (`sub`) and each entry at the one before it (`prev`), so the import walks
//! from the root and needs to seek; blocks are decompressed on demand and the last few kept.
//!
//! Entries are mapped as in the JSON import ([`super`]): the CBOR fields fill the same info
//! block. Every reference is checked against the file and its block, and an entry reached
//! twice (a loop, or one entry in two directories) is refused.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use dfdu_tree::{Ix, Tree};

use super::{
    Imported, Metadata, add_entry, add_root, check_depth, check_entries, close_dir, valid_name,
};
use crate::{ImportError, Limits};

mod entry;

use entry::{Cbor, Item, Ref};

/// The first bytes of a binary export.
pub const SIGNATURE: &[u8; 8] = b"\xbfncduEX1";

/// Whether `head` starts like a binary export.
#[must_use]
pub fn is_binary(head: &[u8]) -> bool {
    head.starts_with(SIGNATURE)
}

/// Largest block, and largest decompressed block: 24-bit lengths and offsets.
const MAX_BLOCK: usize = (1 << 24) - 1;
/// Decompressed blocks kept: the walk mostly stays within a few.
const CACHE: usize = 8;

const DATA: u32 = 0;
const INDEX: u32 = 1;

fn bad(offset: u64, msg: impl Into<String>) -> ImportError {
    ImportError::Format {
        offset: Some(offset),
        msg: msg.into(),
    }
}

/// Reads a binary export from a seekable source positioned anywhere.
///
/// # Errors
///
/// When `r` fails, the export is malformed, or it exceeds `limits`.
pub fn import(mut r: impl Read + Seek, limits: &Limits) -> Result<Imported, ImportError> {
    let len = r.seek(SeekFrom::End(0))?;
    let mut sig = [0u8; 8];
    r.seek(SeekFrom::Start(0))?;
    r.read_exact(&mut sig)
        .map_err(|_| bad(0, "too short for a binary export"))?;
    if !is_binary(&sig) {
        return Err(bad(0, "not an ncdu binary export"));
    }
    let blocks = read_index(&mut r, len)?;
    let mut f = File {
        r,
        blocks: blocks.pointers,
        cache: Vec::with_capacity(CACHE),
        tick: 0,
    };
    walk(&mut f, blocks.root, limits)
}

struct Index {
    /// Offset and length of each data block by number; zero length for none.
    pointers: Vec<(u64, u32)>,
    root: u64,
}

fn type_len(b: [u8; 4]) -> (u32, u32) {
    let v = u32::from_be_bytes(b);
    (v >> 28, v & 0x0fff_ffff)
}

fn read_index(r: &mut (impl Read + Seek), len: u64) -> Result<Index, ImportError> {
    if len < 8 + 8 + 16 {
        return Err(bad(len, "too short for a binary export"));
    }
    let mut foot = [0u8; 4];
    r.seek(SeekFrom::Start(len - 4))?;
    r.read_exact(&mut foot)?;
    let (kind, blen) = type_len(foot);
    let blen = u64::from(blen);
    if kind != INDEX {
        return Err(bad(len - 4, "the last block is not the index"));
    }
    if blen < 16 || blen % 8 != 0 || blen > len - 8 || blen > MAX_BLOCK as u64 {
        return Err(bad(len - 4, format!("bad index length {blen}")));
    }
    let start = len - blen;
    let size = usize::try_from(blen).map_err(|_| bad(len - 4, "index too large"))?;
    let mut block = vec![0u8; size];
    r.seek(SeekFrom::Start(start))?;
    r.read_exact(&mut block)?;
    if block[..4] != foot {
        return Err(bad(start, "index header and footer differ"));
    }
    let body = &block[4..block.len() - 4];
    let Some((ptrs, &root)) = body.split_last_chunk::<8>() else {
        return Err(bad(start, "index without a root"));
    };
    let root = u64::from_be_bytes(root);
    let pointers = ptrs
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&c| {
            let v = u64::from_be_bytes(c);
            (v >> 24, (v & 0xff_ffff) as u32)
        })
        .collect();
    Ok(Index { pointers, root })
}

struct Block {
    num: u32,
    data: Vec<u8>,
    used: u64,
}

struct File<R> {
    r: R,
    blocks: Vec<(u64, u32)>,
    cache: Vec<Block>,
    tick: u64,
}

impl<R: Read + Seek> File<R> {
    /// The decompressed data block `num`, read and kept if it is not yet.
    fn block(&mut self, num: u64) -> Result<&[u8], ImportError> {
        self.tick += 1;
        if let Some(i) = self.cache.iter().position(|b| u64::from(b.num) == num) {
            self.cache[i].used = self.tick;
            return Ok(&self.cache[i].data);
        }
        let &(off, blen) = usize::try_from(num)
            .ok()
            .and_then(|n| self.blocks.get(n))
            .filter(|&&(_, l)| l != 0)
            .ok_or_else(|| bad(0, format!("no data block {num}")))?;
        if blen < 12 || off < 8 {
            return Err(bad(off, format!("bad length {blen} for block {num}")));
        }
        let mut raw = vec![0u8; blen as usize];
        self.r.seek(SeekFrom::Start(off))?;
        self.r
            .read_exact(&mut raw)
            .map_err(|_| bad(off, format!("block {num} is cut short")))?;
        let head = [raw[0], raw[1], raw[2], raw[3]];
        if type_len(head) != (DATA, blen) || raw[raw.len() - 4..] != head {
            return Err(bad(
                off,
                format!("block {num}'s framing does not match the index"),
            ));
        }
        let stored = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        if u64::from(stored) != num {
            return Err(bad(off, format!("block {num} says it is block {stored}")));
        }
        let mut data = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(&raw[8..raw.len() - 4])
            .map_err(|e| bad(off, format!("block {num}: {e}")))?
            .take(MAX_BLOCK as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| bad(off, format!("block {num}: {e}")))?;
        if data.len() > MAX_BLOCK {
            return Err(bad(off, format!("block {num} decompresses to over 16 MiB")));
        }
        let b = Block {
            num: stored,
            data,
            used: self.tick,
        };
        let i = if self.cache.len() < CACHE {
            self.cache.push(b);
            self.cache.len() - 1
        } else {
            // The least recently used; `CACHE` is not 0, so there is one.
            let i = (0..CACHE).min_by_key(|&i| self.cache[i].used).unwrap_or(0);
            self.cache[i] = b;
            i
        };
        Ok(&self.cache[i].data)
    }

    /// The entry at absolute reference `at`, with its `prev` and `sub` made absolute.
    fn item(&mut self, at: u64, limits: &Limits) -> Result<Item, ImportError> {
        let (num, off) = (at >> 24, (at & 0xff_ffff) as usize);
        let data = self.block(num)?;
        if off >= data.len() {
            return Err(bad(0, format!("reference past the end of block {num}")));
        }
        let mut c = Cbor {
            b: data,
            pos: off,
            block: num,
        };
        let raw = c.item(limits)?;
        let abs = |rel: Ref| -> Result<u64, ImportError> {
            match rel {
                Ref::Abs(v) => Ok(v),
                Ref::Rel(back) => off
                    .checked_sub(usize::try_from(back).unwrap_or(usize::MAX))
                    .map(|o| (num << 24) | o as u64)
                    .ok_or_else(|| bad(0, format!("reference before the start of block {num}"))),
            }
        };
        Ok(Item {
            prev: raw.prev.map(abs).transpose()?,
            sub: raw.sub.map(abs).transpose()?,
            typ: raw.typ,
            info: raw.info,
            rderr: raw.rderr,
        })
    }
}

fn walk<R: Read + Seek>(
    f: &mut File<R>,
    root_ref: u64,
    limits: &Limits,
) -> Result<Imported, ImportError> {
    // Every entry reached, so that a loop or an entry in two directories is refused rather
    // than read forever.
    let mut seen: HashSet<u64> = HashSet::new();
    seen.insert(root_ref);
    let item = f.item(root_ref, limits)?;
    let sub = item.sub;
    let (info, listed) = item.into_info();
    if !listed {
        return Err(bad(0, "the root is not a directory"));
    }
    if info.name.is_empty() || info.name.contains(&0) {
        return Err(bad(0, "bad root name"));
    }
    let mut tree = Tree::new();
    let mut links: HashSet<(u64, u64)> = HashSet::new();
    let (root, dev) = add_root(&mut tree, &info);
    let mut entries = 1u64;
    // Each open directory, its device and the next entry of it to read (they are read last
    // first).
    let mut stack: Vec<(Ix, u64, Option<u64>)> = vec![(root, dev, sub)];
    while let Some(top) = stack.last_mut() {
        let (dir, pdev) = (top.0, top.1);
        let Some(at) = top.2 else {
            close_dir(&mut tree, dir);
            stack.pop();
            continue;
        };
        if !seen.insert(at) {
            return Err(bad(0, format!("entry {at:#x} is reached twice")));
        }
        entries += 1;
        check_entries(entries, limits)?;
        let item = f.item(at, limits)?;
        top.2 = item.prev;
        let sub = item.sub;
        let (info, listed) = item.into_info();
        if !valid_name(&info.name) {
            return Err(bad(0, format!("bad name \"{}\"", info.name.escape_ascii())));
        }
        if listed {
            check_depth(stack.len(), limits)?;
        }
        let ix = add_entry(&mut tree, &mut links, dir, pdev, &info, listed);
        if listed {
            stack.push((ix, info.dev.unwrap_or(pdev), sub));
        }
    }
    Ok(Imported {
        tree,
        root,
        minor: 0,
        meta: Metadata::default(),
        entries,
    })
}

#[cfg(test)]
mod tests;
