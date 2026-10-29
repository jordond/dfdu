//! The simulated filesystem: a seeded random tree of directories, files, links, mounts and
//! subvolumes, with the faults its reads will hit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{Kind, Meta, Subvol};
use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

mod extents;
mod generate;

pub use generate::SpecConfig;

/// Seeded `ChaCha8`: the stream for a seed is fixed by the algorithm, so a failing seed replays
/// the same run on every platform.
pub struct Rng(ChaCha8Rng);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Rng(ChaCha8Rng::seed_from_u64(seed))
    }
    /// Uniform in `0..n`; 0 when `n` is 0.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.0.random_range(0..n) }
    }
    /// True with probability `per_mille / 1000`.
    pub fn chance(&mut self, per_mille: u32) -> bool {
        self.0.random_ratio(per_mille, 1000)
    }
    /// [`Rng::below`] for an index: uniform in `0..n`; 0 when `n` is 0. Draws as `below` does,
    /// so a seed gives the same run.
    pub fn index(&mut self, n: usize) -> usize {
        // Below `n`, so it fits in `usize`.
        usize::try_from(self.below(n as u64)).unwrap_or(0)
    }
}

/// A spec node or filesystem number as the `u32` the spec stores.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a generated spec has a few thousand nodes at most"
)]
fn id32(n: usize) -> u32 {
    n as u32
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Opening the directory fails with permission denied.
    Eacces,
    /// Reading fails with EIO after this many entries.
    EioAfter(usize),
    /// Reading panics when it reaches this block.
    PanicAtBlock(usize),
    PanicOnOpen,
    /// Reading blocks when it reaches `block`, for `dur_ns` of virtual time, or forever.
    Stall {
        block: usize,
        dur_ns: Option<u64>,
    },
    /// Stat'ing block `.0` panics, wherever it is stat'ed (by the listing task or a chunk
    /// task); the other blocks are read.
    StatPanic(usize),
    /// Stat'ing `block` blocks for `dur_ns`, or forever, wherever it is stat'ed.
    StatStall {
        block: usize,
        dur_ns: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct SpecNode {
    pub name: Vec<u8>,
    pub meta: Meta,
    pub children: Vec<u32>,
    pub fault: Fault,
    /// The entry disappears between listing and stat (files only).
    pub vanished: bool,
    /// The directory is a btrfs subvolume root.
    pub subvol: Option<Subvol>,
    /// A second mount of another directory in the spec (the backend reports it as such).
    pub dup_mount: bool,
    /// Mount root of a network or kernel filesystem (others are local).
    pub fs_class: Option<FsClass>,
    /// What the directory's filesystem is stored on (directories only), for mount limits.
    pub storage: Storage,
    /// Extent map of a file on a filesystem that shares extents.
    pub pieces: Vec<SimPiece>,
    /// Measuring the file fails (it cannot be opened).
    pub unmeasurable: bool,
    /// What reading the file gives (rule markers such as CACHEDIR.TAG).
    pub content: Vec<u8>,
}

impl SpecNode {
    /// An entry with no children, faults, mounts or contents, on unknown storage.
    #[must_use]
    pub fn new(name: Vec<u8>, meta: Meta) -> SpecNode {
        SpecNode {
            name,
            meta,
            children: vec![],
            fault: Fault::None,
            vanished: false,
            subvol: None,
            dup_mount: false,
            fs_class: None,
            storage: Storage::Unknown,
            pieces: vec![],
            unmeasurable: false,
            content: vec![],
        }
    }
}

/// One extent of a simulated file. Compressed extents are identified by
/// their physical start; `len` is uncompressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimPiece {
    pub phys: u64,
    pub len: u64,
    pub encoded: bool,
    /// What FIEMAP reports: another file references the data, inside the spec or (randomly)
    /// outside it.
    pub shared: bool,
}

pub struct Spec {
    pub nodes: Vec<SpecNode>,
    /// Filesystem of each device (subvolumes share their parent's), and whether it shares
    /// extents.
    pub dev_fs: HashMap<u64, (u32, bool)>,
    pub parent: Vec<u32>,
    /// Set when the simulated deleter removes the entry.
    pub deleted: Vec<AtomicBool>,
}

impl Spec {
    #[must_use]
    pub fn is_deleted(&self, i: u32) -> bool {
        self.deleted[i as usize].load(SeqCst)
    }

    /// Makes a removed entry exist again (a scenario replacing a file by another of the same
    /// name removes one and restores the other).
    pub fn restore(&self, i: u32) {
        self.deleted[i as usize].store(false, SeqCst);
    }

    pub fn delete_subtree(&self, i: u32) {
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            self.deleted[n as usize].store(true, SeqCst);
            stack.extend(&self.nodes[n as usize].children);
        }
    }

    /// Entries the simulated deleter may remove: their parent's listing has no fault, so
    /// removing them cannot shift a fault's position (see `run`).
    #[must_use]
    pub fn deletable(&self, i: u32) -> bool {
        i != ROOT
            && !self.is_deleted(i)
            && self.nodes[self.parent[i as usize] as usize].fault == Fault::None
    }

    pub fn live_children(&self, i: u32) -> impl Iterator<Item = u32> + '_ {
        self.nodes[i as usize]
            .children
            .iter()
            .copied()
            .filter(|&c| !self.is_deleted(c))
    }
}

pub const ROOT: u32 = 0;
pub const ROOT_NAME: &str = "/sim";

/// The CACHEDIR.TAG signature (bford.info/cachedir).
pub const CACHEDIR_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n";

/// Builds a spec by hand for scenario tests: one device, mount ids, no shared extents.
/// Directories and files get unique inode numbers; [`SpecBuilder::link`] adds hard links.
pub struct SpecBuilder {
    nodes: Vec<SpecNode>,
    ino: u64,
    absent: Vec<u32>,
}

impl Default for SpecBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SpecBuilder {
    #[must_use]
    pub fn new() -> Self {
        let mut b = SpecBuilder {
            nodes: vec![],
            ino: 1000,
            absent: vec![],
        };
        b.push(ROOT_NAME, Kind::Dir, 4096);
        b
    }

    fn push(&mut self, name: &str, kind: Kind, size: u64) -> u32 {
        self.ino += 1;
        let meta = Meta {
            kind,
            apparent: size,
            alloc: size.div_ceil(4096) * 4096,
            mtime: 1,
            dev: 1,
            ino: self.ino,
            nlink: if kind == Kind::Dir { 2 } else { 1 },
            mount: 1,
        };
        self.nodes
            .push(SpecNode::new(name.as_bytes().to_vec(), meta));
        id32(self.nodes.len() - 1)
    }

    fn add(&mut self, parent: u32, name: &str, kind: Kind, size: u64) -> u32 {
        let i = self.push(name, kind, size);
        self.nodes[parent as usize].children.push(i);
        i
    }

    pub fn dir(&mut self, parent: u32, name: &str) -> u32 {
        self.add(parent, name, Kind::Dir, 4096)
    }

    pub fn file(&mut self, parent: u32, name: &str, size: u64) -> u32 {
        self.add(parent, name, Kind::File, size)
    }

    /// `n` files named `{prefix}{i}`, of sizes 1000, 1001, ...
    pub fn files(&mut self, parent: u32, prefix: &str, n: usize) -> Vec<u32> {
        (0..n)
            .map(|i| self.file(parent, &format!("{prefix}{i}"), 1000 + i as u64))
            .collect()
    }

    /// A hard link to file `target` named `name` in `parent`.
    pub fn link(&mut self, parent: u32, name: &str, target: u32) -> u32 {
        let meta = self.nodes[target as usize].meta;
        let i = self.add(parent, name, Kind::File, meta.apparent);
        self.nodes[i as usize].meta = meta;
        i
    }

    /// What reading file `i` gives.
    pub fn content(&mut self, i: u32, bytes: &[u8]) {
        self.nodes[i as usize].content = bytes.to_vec();
    }

    pub fn fault(&mut self, dir: u32, fault: Fault) {
        self.nodes[dir as usize].fault = fault;
    }

    /// The entry starts out removed; [`Spec::restore`] creates it.
    pub fn absent(&mut self, i: u32) {
        self.absent.push(i);
    }

    #[must_use]
    pub fn build(mut self) -> Spec {
        let mut links: HashMap<u64, u32> = HashMap::new();
        for n in self.nodes.iter().filter(|n| n.meta.kind == Kind::File) {
            *links.entry(n.meta.ino).or_default() += 1;
        }
        for n in self.nodes.iter_mut().filter(|n| n.meta.kind == Kind::File) {
            n.meta.nlink = links[&n.meta.ino];
        }
        let mut parent = vec![ROOT; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            for &c in &n.children {
                parent[c as usize] = id32(i);
            }
        }
        let deleted = (0..self.nodes.len())
            .map(|i| AtomicBool::new(self.absent.contains(&id32(i))))
            .collect();
        Spec {
            nodes: self.nodes,
            dev_fs: HashMap::from([(1, (0, false))]),
            parent,
            deleted,
        }
    }
}

#[cfg(test)]
mod tests;
