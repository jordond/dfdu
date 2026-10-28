//! A scanner backend serving a [`Plan`] from memory: the same directories, names, sizes, hard
//! links and symlinks `dfdu-bench gen` would create, without creating them. The applier's cost
//! depends on the messages it receives, not on where the workers read them, so this measures
//! the applier at 10M entries with RAM instead of 10M inodes.
//!
//! Directories are listed subdirectories first, then the other entries in plan order. Inode
//! numbers: directory `d` is `d + 1`; files are numbered after the directories in plan order,
//! and a hard link has its target's number.

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Arc;

use dfdu_scan::{Backend, Block, DirStream, FsClass, Raw, RawBlock, Storage};
use dfdu_tree::{Kind, Meta};

use crate::{Entry, Plan, index};

/// Path of the root of a synthetic tree.
pub const ROOT: &str = "/synth";

const DEV: u64 = 1;
const MOUNT: u64 = 1;
const MTIME: i64 = 1_700_000_000;

/// A plan with the indexes the backend needs.
pub struct SynthTree {
    plan: Plan,
    /// Subdirectories of directory `d`: `subdirs[sub_start[d]..sub_start[d + 1]]`.
    sub_start: Vec<u32>,
    subdirs: Vec<u32>,
    /// Inode number of directory `d`'s first file.
    first_ino: Vec<u64>,
    /// Link count of data files that have hard links, by (directory, entry).
    nlink: HashMap<(u32, u32), u32>,
    /// CPU time each entry costs to stat, spent spinning ([`SynthBackend::with_stat_ns`]).
    stat_ns: u64,
}

impl SynthTree {
    #[must_use]
    pub fn new(plan: Plan) -> SynthTree {
        let n = plan.dirs.len();
        let mut count = vec![0u32; n + 1];
        for d in &plan.dirs[1..] {
            count[d.parent as usize + 1] += 1;
        }
        for i in 0..n {
            count[i + 1] += count[i];
        }
        let sub_start = count.clone();
        let mut subdirs = vec![0u32; n.saturating_sub(1)];
        let mut next = count;
        for (i, d) in plan.dirs.iter().enumerate().skip(1) {
            let p = d.parent as usize;
            subdirs[next[p] as usize] = index(i);
            next[p] += 1;
        }
        let mut first_ino = Vec::with_capacity(n);
        let mut ino = n as u64 + 1;
        let mut nlink = HashMap::new();
        for d in &plan.dirs {
            first_ino.push(ino);
            ino += d.entries.len() as u64;
            for e in &d.entries {
                if let Entry::HardLink(ld, lf) = *e {
                    *nlink.entry((ld, lf)).or_insert(1) += 1;
                }
            }
        }
        SynthTree {
            plan,
            sub_start,
            subdirs,
            first_ino,
            nlink,
            stat_ns: 0,
        }
    }

    /// Entries in the tree, the root not included.
    #[must_use]
    pub fn entries(&self) -> u64 {
        (self.plan.dirs.len() - 1) as u64
            + self
                .plan
                .dirs
                .iter()
                .map(|d| d.entries.len() as u64)
                .sum::<u64>()
    }

    fn subdirs(&self, d: u32) -> &[u32] {
        let d = d as usize;
        &self.subdirs[self.sub_start[d] as usize..self.sub_start[d + 1] as usize]
    }

    fn dir_meta(&self, d: u32) -> Meta {
        Meta {
            kind: Kind::Dir,
            apparent: 4096,
            alloc: 4096,
            mtime: MTIME,
            dev: DEV,
            ino: u64::from(d) + 1,
            nlink: 2 + index(self.subdirs(d).len()),
            mount: MOUNT,
        }
    }

    fn file_meta(&self, d: u32, k: u32) -> Meta {
        let entry = &self.plan.dirs[d as usize].entries[k as usize];
        let (kind, apparent, alloc, ino, nlink) = match *entry {
            Entry::Data(size) => {
                let nlink = self.nlink.get(&(d, k)).copied().unwrap_or(1);
                (
                    Kind::File,
                    size,
                    alloc(size),
                    self.first_ino[d as usize] + u64::from(k),
                    nlink,
                )
            }
            Entry::HardLink(ld, lf) => {
                let Entry::Data(size) = self.plan.dirs[ld as usize].entries[lf as usize] else {
                    unreachable!("hard links point at data files")
                };
                (
                    Kind::File,
                    size,
                    alloc(size),
                    self.first_ino[ld as usize] + u64::from(lf),
                    self.nlink[&(ld, lf)],
                )
            }
            Entry::Symlink(ref t) => (
                Kind::Symlink,
                t.len() as u64,
                0,
                self.first_ino[d as usize] + u64::from(k),
                1,
            ),
        };
        Meta {
            kind,
            apparent,
            alloc,
            mtime: MTIME,
            dev: DEV,
            ino,
            nlink,
            mount: 0,
        }
    }
}

/// Small files are written, larger ones sparse (as `dfdu-bench gen` makes them).
fn alloc(size: u64) -> u64 {
    if size > 0 && size <= 512 { 4096 } else { 0 }
}

/// The backend. Paths other than [`ROOT`] are not supported (rescans of subdirectories).
#[derive(Clone)]
pub struct SynthBackend(pub Arc<SynthTree>);

impl SynthBackend {
    /// The tree `dfdu-bench gen --entries ENTRIES --seed SEED --wide WIDE` creates.
    #[must_use]
    pub fn generate(entries: u64, seed: u64, wide: u64) -> SynthBackend {
        SynthBackend(Arc::new(SynthTree::new(Plan::generate(
            entries, seed, wide,
        ))))
    }

    /// Makes each entry cost `ns` of CPU time to stat, as a real `statx` does (1-2 µs with a
    /// warm cache), so workers compete with the applier for CPUs as in a real scan. Without
    /// it workers produce entries much faster than the applier takes them.
    ///
    /// # Panics
    ///
    /// If the backend was cloned before this call: the cost is set on the tree the clones share.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "setting the cost on a shared tree would change the clones too; a builder misuse"
    )]
    pub fn with_stat_ns(mut self, ns: u64) -> SynthBackend {
        Arc::get_mut(&mut self.0).expect("not shared yet").stat_ns = ns;
        self
    }
}

pub struct SynthStream {
    tree: Arc<SynthTree>,
    dir: u32,
    pos: usize,
    len: usize,
}

impl DirStream for SynthStream {
    type Loc = u32;

    fn next_raw(
        &mut self,
        max_entries: usize,
        _max_name_bytes: usize,
    ) -> io::Result<(Raw<u32>, bool)> {
        let end = (self.pos + max_entries).min(self.len);
        let raw = SynthRaw {
            tree: self.tree.clone(),
            dir: self.dir,
            items: self.pos..end,
            name: Vec::new(),
        };
        self.pos = end;
        Ok((Box::new(raw), self.pos < self.len))
    }
}

/// Entries `items` of a directory: subdirectories first, then the plan's entries.
struct SynthRaw {
    tree: Arc<SynthTree>,
    dir: u32,
    items: std::ops::Range<usize>,
    name: Vec<u8>,
}

impl RawBlock<u32> for SynthRaw {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn stat(&mut self, out: &mut Block<u32>) -> io::Result<()> {
        let t = &*self.tree;
        let subs = t.subdirs(self.dir);
        let start = std::time::Instant::now();
        for (n, i) in self.items.clone().enumerate() {
            if t.stat_ns > 0 {
                let until = std::time::Duration::from_nanos(t.stat_ns * n as u64);
                while start.elapsed() < until {
                    std::hint::spin_loop();
                }
            }
            if let Some(&c) = subs.get(i) {
                let name = t.plan.dirs[c as usize].name.as_bytes();
                out.push(name, Ok(t.dir_meta(c)), Some(c));
            } else {
                let k = index(i - subs.len());
                self.name.clear();
                let _ = match t.plan.dirs[self.dir as usize].entries[k as usize] {
                    Entry::Data(_) => write!(self.name, "f{k:x}.dat"),
                    Entry::HardLink(..) => write!(self.name, "h{k:x}.dat"),
                    Entry::Symlink(_) => write!(self.name, "l{k:x}"),
                };
                out.push(&self.name, Ok(t.file_meta(self.dir, k)), None);
            }
        }
        self.items = self.items.end..self.items.end;
        Ok(())
    }
}

impl Backend for SynthBackend {
    type Loc = u32;
    type Stream = SynthStream;

    fn loc_from_path(&self, path: &Path) -> u32 {
        assert_eq!(path, Path::new(ROOT), "only the root of a synthetic tree");
        0
    }

    fn stat_root(&self, loc: &u32) -> io::Result<Meta> {
        Ok(self.0.dir_meta(*loc))
    }

    fn open(&self, loc: &u32, _storage: Storage) -> io::Result<SynthStream> {
        let t = &self.0;
        let len = t.subdirs(*loc).len() + t.plan.dirs[*loc as usize].entries.len();
        Ok(SynthStream {
            tree: t.clone(),
            dir: *loc,
            pos: 0,
            len,
        })
    }

    fn mount_class(&self, _mount: u64, _dev: u64) -> Option<FsClass> {
        Some(FsClass::Local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry is listed once, with the names `dfdu-bench gen` would give it.
    #[test]
    fn lists_the_plan() {
        let b = SynthBackend::generate(20_000, 3, 2_000);
        let mut seen = 0u64;
        let mut links = 0;
        let mut stack = vec![0u32];
        while let Some(d) = stack.pop() {
            let mut s = b.open(&d, Storage::Unknown).unwrap();
            loop {
                let (mut raw, more) = s.next_raw(700, usize::MAX).unwrap();
                let mut block = Block::default();
                raw.stat(&mut block).unwrap();
                for e in &block.entries {
                    seen += 1;
                    let m = e.meta.as_ref().unwrap();
                    links += u64::from(m.is_hardlink());
                    if let Some(c) = e.child {
                        assert_eq!(
                            block.name(e.name),
                            b.0.plan.dirs[c as usize].name.as_bytes()
                        );
                        stack.push(c);
                    }
                }
                if !more {
                    break;
                }
            }
        }
        assert_eq!(seen, 20_000);
        assert_eq!(seen, b.0.entries());
        assert!(links > 100, "{links} hard links");
    }
}
