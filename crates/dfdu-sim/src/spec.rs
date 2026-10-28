//! The simulated filesystem: a seeded random tree of directories, files, links, mounts and
//! subvolumes, with the faults its reads will hit.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use dfdu_scan::{FsClass, Storage};
use dfdu_tree::{Kind, Meta, Subvol};
use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

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

#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches of the generator, each a feature a run may leave out"
)]
pub struct SpecConfig {
    pub max_depth: u32,
    pub max_dirs: u64,
    pub max_files: u64,
    pub faults: bool,
    pub hardlinks: bool,
    /// Directories on other mounts (new `dev` and mount id).
    pub other_fs: bool,
    /// btrfs subvolume roots (new `dev`, same mount id, inode 256).
    pub subvols: bool,
    /// The backend reports mount ids; without them `dev` decides, and subvolumes look like
    /// other filesystems (Linux before 5.8, other systems).
    pub mount_ids: bool,
    /// Second mounts (copies) of some mounts, next to them.
    pub dup_mounts: bool,
    /// Chance per directory below the root, per mille, of being another mount's root.
    pub mount_per_mille: u64,
    /// Extent maps, reflinked copies and snapshots that share data.
    pub extents: bool,
    /// Names the built-in rules match (`target`, `Cargo.toml`, CACHEDIR.TAG) among the
    /// generated ones.
    pub rule_names: bool,
}

impl Default for SpecConfig {
    fn default() -> Self {
        SpecConfig {
            max_depth: 4,
            max_dirs: 4,
            max_files: 7,
            faults: true,
            hardlinks: true,
            other_fs: true,
            subvols: true,
            mount_ids: true,
            dup_mounts: true,
            mount_per_mille: 30,
            extents: true,
            rule_names: false,
        }
    }
}

/// Names that rules ask about, given to generated entries with [`SpecConfig::rule_names`]: the
/// `i`th entry may get the `i`th name, so names stay unique in a directory.
const DIR_NAMES: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "debug",
    "cache",
    "release",
];
const FILE_NAMES: &[&str] = &[
    "Cargo.toml",
    "CACHEDIR.TAG",
    "package.json",
    "pyvenv.cfg",
    "yarn.lock",
    "requirements.txt",
    ".rustc_info.json",
];

/// The CACHEDIR.TAG signature (bford.info/cachedir).
pub const CACHEDIR_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n";

fn pick_name(rng: &mut Rng, cfg: &SpecConfig, pool: &[&str], prefix: &str, i: u64) -> Vec<u8> {
    match usize::try_from(i).ok().and_then(|i| pool.get(i)) {
        Some(n) if cfg.rule_names && rng.chance(600) => n.as_bytes().to_vec(),
        _ => format!("{prefix}{i}").into_bytes(),
    }
}

impl Spec {
    #[expect(
        clippy::too_many_lines,
        reason = "one pass of seeded draws, whose order decides each seed's tree"
    )]
    pub fn generate(rng: &mut Rng, cfg: &SpecConfig, block: usize) -> Spec {
        // Above the subvolume root inode number (256), so it is unique within a device.
        let mut ino = 1000u64;
        let (mut next_dev, mut next_mount) = (1u64, 1u64);
        let mut mount_dirs = vec![];
        let mut subvol_dirs = vec![];
        // The root filesystem shares extents in most runs; other mounts in some.
        let mut dev_fs: HashMap<u64, (u32, bool)> =
            HashMap::from([(1, (0, cfg.extents && rng.chance(800)))]);
        let mut nodes = vec![SpecNode {
            name: ROOT_NAME.as_bytes().to_vec(),
            meta: Meta {
                kind: Kind::Dir,
                apparent: 4096,
                alloc: 4096,
                mtime: 1,
                dev: 1,
                ino,
                nlink: 2,
                mount: u64::from(cfg.mount_ids),
            },
            children: vec![],
            fault: Fault::None,
            vanished: false,
            subvol: None,
            dup_mount: false,
            fs_class: None,
            storage: [
                Storage::Solid,
                Storage::Rotational,
                Storage::Network,
                Storage::Unknown,
            ][rng.index(4)],
            pieces: vec![],
            unmeasurable: false,
            content: vec![],
        }];
        let mut queue = VecDeque::from([(ROOT, 0u32)]);
        while let Some((d, depth)) = queue.pop_front() {
            let dev = nodes[d as usize].meta.dev;
            let ndirs = if depth < cfg.max_depth {
                rng.below(cfg.max_dirs + 1)
            } else {
                0
            };
            let nfiles = rng.below(cfg.max_files + 1);
            let mut kids = vec![];
            let mount = nodes[d as usize].meta.mount;
            let parent_storage = nodes[d as usize].storage;
            for i in 0..ndirs {
                ino += 1;
                let roll = rng.below(1000);
                let mut fs_class = None;
                let mut storage = parent_storage;
                let other = cfg.mount_per_mille;
                let (cdev, cmount, cino, subvol) = if cfg.other_fs && depth > 0 && roll < other {
                    fs_class = match rng.below(12) {
                        0 => Some(FsClass::Kernel),
                        1 | 2 => Some(FsClass::Network),
                        _ => None,
                    };
                    storage = match (fs_class, rng.below(4)) {
                        (Some(FsClass::Network), _) => Storage::Network,
                        (_, 0 | 1) => Storage::Rotational,
                        (_, 2) => Storage::Solid,
                        _ => Storage::Unknown,
                    };
                    next_dev += 1;
                    next_mount += 1;
                    mount_dirs.push(id32(nodes.len()));
                    let fs = id32(dev_fs.len());
                    dev_fs.insert(next_dev, (fs, cfg.extents && rng.chance(500)));
                    let m = if cfg.mount_ids { next_mount } else { 0 };
                    (next_dev, m, ino, None)
                } else if cfg.subvols && roll < other + 40 {
                    next_dev += 1;
                    dev_fs.insert(next_dev, dev_fs[&dev]);
                    subvol_dirs.push(id32(nodes.len()));
                    let sv = Subvol {
                        id: 256 + next_dev,
                        uuid: [(next_dev & 0xff) as u8; 16],
                        parent_uuid: rng.chance(500).then_some([1; 16]),
                        read_only: rng.chance(333),
                        otime: 1,
                        qgroup: None,
                    };
                    (next_dev, mount, 256, Some(sv))
                } else {
                    (dev, mount, ino, None)
                };
                let id = id32(nodes.len());
                kids.push(id);
                nodes.push(SpecNode {
                    name: pick_name(rng, cfg, DIR_NAMES, "d", i),
                    meta: Meta {
                        kind: Kind::Dir,
                        apparent: 4096,
                        alloc: 4096,
                        mtime: 1,
                        dev: cdev,
                        ino: cino,
                        nlink: 2,
                        mount: cmount,
                    },
                    children: vec![],
                    fault: Fault::None,
                    vanished: false,
                    subvol,
                    dup_mount: false,
                    fs_class,
                    storage,
                    pieces: vec![],
                    unmeasurable: false,
                    content: vec![],
                });
                queue.push_back((id, depth + 1));
            }
            for i in 0..nfiles {
                ino += 1;
                let size = rng.below(100_000);
                let alloc = if rng.chance(100) {
                    0
                } else {
                    size.div_ceil(4096) * 4096
                };
                kids.push(id32(nodes.len()));
                let name = pick_name(rng, cfg, FILE_NAMES, "f", i);
                let content = match &name[..] {
                    b"CACHEDIR.TAG" if rng.chance(700) => CACHEDIR_TAG.to_vec(),
                    b"CACHEDIR.TAG" => b"Signature: not this one".to_vec(),
                    _ => vec![],
                };
                nodes.push(SpecNode {
                    name,
                    meta: Meta {
                        kind: Kind::File,
                        apparent: size,
                        alloc,
                        mtime: 1,
                        dev,
                        ino,
                        nlink: 1,
                        mount: 0,
                    },
                    children: vec![],
                    fault: Fault::None,
                    vanished: cfg.faults && rng.chance(30),
                    subvol: None,
                    dup_mount: false,
                    fs_class: None,
                    storage: Storage::Unknown,
                    pieces: vec![],
                    unmeasurable: cfg.faults && rng.chance(30),
                    content,
                });
            }
            nodes[d as usize].children = kids;
        }
        // Hard links: some files become extra links of another file's inode (same device).
        if cfg.hardlinks {
            let files: Vec<u32> = (0..id32(nodes.len()))
                .filter(|&i| {
                    nodes[i as usize].meta.kind == Kind::File && !nodes[i as usize].vanished
                })
                .collect();
            for _ in 0..files.len() / 4 {
                let a = files[rng.index(files.len())] as usize;
                let b = files[rng.index(files.len())] as usize;
                if a == b || nodes[a].meta.dev != nodes[b].meta.dev {
                    continue;
                }
                let m = nodes[a].meta;
                nodes[b].meta = Meta { nlink: 1, ..m };
            }
            let mut count: HashMap<(u64, u64), u32> = HashMap::new();
            for n in nodes.iter().filter(|n| n.meta.kind == Kind::File) {
                *count.entry((n.meta.dev, n.meta.ino)).or_default() += 1;
            }
            for n in nodes.iter_mut().filter(|n| n.meta.kind == Kind::File) {
                let c = count[&(n.meta.dev, n.meta.ino)];
                // Some inodes have links outside the scanned tree as well.
                n.meta.nlink = if c > 1 {
                    c + (n.meta.ino % 2) as u32
                } else {
                    1
                };
            }
        }
        if cfg.faults {
            for node in nodes.iter_mut().skip(1) {
                if node.meta.kind != Kind::Dir {
                    continue;
                }
                let len = node.children.len();
                let last_block = len.saturating_sub(1) / block;
                let r = rng.below(1000);
                node.fault = match r {
                    0..50 => Fault::Eacces,
                    50..90 => Fault::EioAfter(rng.index(len + 1)),
                    90..110 if len > 0 => Fault::PanicAtBlock(rng.index(last_block + 1)),
                    110..120 => Fault::PanicOnOpen,
                    120..150 => Fault::Stall {
                        block: rng.index(last_block + 1),
                        dur_ns: Some(20_000_000 + rng.below(200_000_000)),
                    },
                    150..170 => Fault::Stall {
                        block: rng.index(last_block + 1),
                        dur_ns: None,
                    },
                    170..190 if len > 0 => Fault::StatPanic(rng.index(last_block + 1)),
                    190..215 if len > 0 => Fault::StatStall {
                        block: rng.index(last_block + 1),
                        dur_ns: Some(20_000_000 + rng.below(200_000_000)),
                    },
                    215..230 if len > 0 => Fault::StatStall {
                        block: rng.index(last_block + 1),
                        dur_ns: None,
                    },
                    _ => Fault::None,
                };
            }
        }
        // Whether a file can be measured belongs to the inode: hard links are one file.
        let mut unmeasurable: HashMap<(u64, u64), bool> = HashMap::new();
        for n in nodes.iter_mut().filter(|n| n.meta.kind == Kind::File) {
            n.unmeasurable = *unmeasurable
                .entry((n.meta.dev, n.meta.ino))
                .or_insert(n.unmeasurable);
        }
        if cfg.extents {
            gen_extents(rng, &mut nodes, &dev_fs, &subvol_dirs);
        }
        // A second mount of some mounts: a copy of the subtree with the same inodes, next to it.
        if cfg.dup_mounts {
            for &d in &mount_dirs {
                if !rng.chance(500) {
                    continue;
                }
                // Mounts replaced by a snapshot copy are no longer in the tree.
                let Some(parent) = (0..nodes.len()).find(|&p| nodes[p].children.contains(&d))
                else {
                    continue;
                };
                let copy = copy_subtree(&mut nodes, d);
                let c = &mut nodes[copy as usize];
                c.name.extend_from_slice(b"-again");
                c.dup_mount = true;
                if cfg.mount_ids {
                    next_mount += 1;
                    c.meta.mount = next_mount;
                }
                nodes[parent].children.push(copy);
            }
        }
        let mut parent = vec![ROOT; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            for &c in &n.children {
                parent[c as usize] = id32(i);
            }
        }
        if cfg.extents {
            mark_shared(rng, &mut nodes);
        }
        let deleted = (0..nodes.len()).map(|_| AtomicBool::new(false)).collect();
        Spec {
            nodes,
            dev_fs,
            parent,
            deleted,
        }
    }
}

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
        self.nodes.push(SpecNode {
            name: name.as_bytes().to_vec(),
            meta: Meta {
                kind,
                apparent: size,
                alloc: size.div_ceil(4096) * 4096,
                mtime: 1,
                dev: 1,
                ino: self.ino,
                nlink: if kind == Kind::Dir { 2 } else { 1 },
                mount: 1,
            },
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
        });
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

/// Extent maps: fresh extents per inode (hard links share them), then reflinked copies of part
/// of another file's data, then snapshots: some subvolumes get a copy of another directory's
/// contents with the same inodes and extents.
fn gen_extents(
    rng: &mut Rng,
    nodes: &mut Vec<SpecNode>,
    dev_fs: &HashMap<u64, (u32, bool)>,
    subvol_dirs: &[u32],
) {
    let mut next_phys: HashMap<u32, u64> = HashMap::new();
    let mut by_inode: HashMap<(u64, u64), Vec<SimPiece>> = HashMap::new();
    let files: Vec<usize> = (0..nodes.len())
        .filter(|&i| {
            let m = &nodes[i].meta;
            m.kind == Kind::File && m.alloc > 0 && dev_fs.get(&m.dev).is_some_and(|d| d.1)
        })
        .collect();
    for &i in &files {
        let m = nodes[i].meta;
        if let Some(p) = by_inode.get(&(m.dev, m.ino)) {
            nodes[i].pieces.clone_from(p);
            continue;
        }
        let fs = dev_fs[&m.dev].0;
        let encoded = rng.chance(100);
        let blocks = m.alloc / 4096;
        let parts = 1 + rng.below(blocks.min(3));
        let mut left = blocks;
        let mut pieces = vec![];
        for k in 0..parts {
            let b = if k + 1 == parts {
                left
            } else {
                1 + rng.below(left - (parts - k - 1))
            };
            left -= b;
            let at = next_phys.entry(fs).or_insert(4096);
            // Compressed extents take less room than their uncompressed length.
            let room = if encoded { 4096 } else { b * 4096 };
            pieces.push(SimPiece {
                phys: *at,
                len: b * 4096,
                encoded,
                shared: false,
            });
            *at += room + 4096 * rng.below(2);
        }
        by_inode.insert((m.dev, m.ino), pieces.clone());
        nodes[i].pieces = pieces;
    }
    // Reflinks: a file whose data is (partly) another file's on the same filesystem.
    for &i in &files {
        if !rng.chance(150) || nodes[i].meta.nlink > 1 {
            continue;
        }
        let j = files[rng.index(files.len())];
        let (fi, fj) = (dev_fs[&nodes[i].meta.dev].0, dev_fs[&nodes[j].meta.dev].0);
        if i == j || fi != fj || nodes[j].pieces.is_empty() {
            continue;
        }
        let src = nodes[j].pieces[rng.index(nodes[j].pieces.len())];
        let piece = if src.encoded {
            src
        } else {
            let blocks = src.len / 4096;
            let skip = rng.below(blocks);
            let take = 1 + rng.below(blocks - skip);
            SimPiece {
                phys: src.phys + skip * 4096,
                len: take * 4096,
                ..src
            }
        };
        let k = rng.index(nodes[i].pieces.len());
        nodes[i].pieces[k] = piece;
    }
    // Snapshots: a subvolume holding a copy of another directory on the same filesystem, with
    // the same inode numbers (on the subvolume's own device) and extents.
    for &sv in subvol_dirs {
        if !rng.chance(400) {
            continue;
        }
        let dirs: Vec<u32> = (0..id32(nodes.len()))
            .filter(|&d| {
                nodes[d as usize].meta.kind == Kind::Dir
                    && d != sv
                    && dev_fs.get(&nodes[d as usize].meta.dev).map(|x| x.0)
                        == dev_fs.get(&nodes[sv as usize].meta.dev).map(|x| x.0)
                    && single_device(nodes, d)
                    && !contains(nodes, d, sv)
                    && !contains(nodes, sv, d)
            })
            .collect();
        if dirs.is_empty() {
            continue;
        }
        let src = dirs[rng.index(dirs.len())];
        let dev = nodes[sv as usize].meta.dev;
        let kids: Vec<u32> = nodes[src as usize].children.clone();
        let copies: Vec<u32> = kids
            .into_iter()
            .map(|c| {
                let n = copy_subtree(nodes, c);
                set_dev(nodes, n, dev);
                n
            })
            .collect();
        nodes[sv as usize].children = copies;
        // Its fault was placed for the listing it had before.
        nodes[sv as usize].fault = Fault::None;
        if let Some(s) = nodes[sv as usize].subvol.as_mut() {
            s.parent_uuid = Some([0xfe; 16]);
        }
    }
}

/// Every node below `d` is on `d`'s device (no mounts or subvolumes inside).
fn single_device(nodes: &[SpecNode], d: u32) -> bool {
    let dev = nodes[d as usize].meta.dev;
    let mut stack = vec![d];
    while let Some(n) = stack.pop() {
        if nodes[n as usize].meta.dev != dev {
            return false;
        }
        stack.extend(&nodes[n as usize].children);
    }
    true
}

fn contains(nodes: &[SpecNode], a: u32, b: u32) -> bool {
    let mut stack = vec![a];
    while let Some(n) = stack.pop() {
        if n == b {
            return true;
        }
        stack.extend(&nodes[n as usize].children);
    }
    false
}

fn set_dev(nodes: &mut [SpecNode], n: u32, dev: u64) {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        nodes[x as usize].meta.dev = dev;
        stack.extend(&nodes[x as usize].children);
    }
}

/// FIEMAP's `SHARED` flag: another inode references overlapping data (the same start, for
/// compressed extents), or, for a few extents, something outside the spec does.
fn mark_shared(rng: &mut Rng, nodes: &mut [SpecNode]) {
    // Reachable files only: orphaned nodes (replaced by snapshot copies) do not exist.
    let mut reach = vec![false; nodes.len()];
    let mut stack = vec![ROOT];
    while let Some(n) = stack.pop() {
        reach[n as usize] = true;
        stack.extend(&nodes[n as usize].children);
    }
    let mut refs: Vec<((u64, u64), SimPiece)> = vec![];
    for (i, n) in nodes.iter().enumerate() {
        if reach[i] {
            for &p in &n.pieces {
                refs.push(((n.meta.dev, n.meta.ino), p));
            }
        }
    }
    // Per inode: hard links are one file and report the same flags.
    let mut flags: HashMap<(u64, u64), Vec<bool>> = HashMap::new();
    for n in nodes.iter_mut() {
        let me = (n.meta.dev, n.meta.ino);
        let f = flags.entry(me).or_insert_with(|| {
            n.pieces
                .iter()
                .map(|p| {
                    let other = refs.iter().any(|(k, q)| {
                        *k != me
                            && q.encoded == p.encoded
                            && if p.encoded {
                                q.phys == p.phys
                            } else {
                                q.phys < p.phys + p.len && p.phys < q.phys + q.len
                            }
                    });
                    other || rng.chance(30)
                })
                .collect()
        });
        for (p, &sh) in n.pieces.iter_mut().zip(f.iter()) {
            p.shared = sh;
        }
    }
}

fn copy_subtree(nodes: &mut Vec<SpecNode>, n: u32) -> u32 {
    let mut node = nodes[n as usize].clone();
    node.children = node
        .children
        .iter()
        .map(|&c| copy_subtree(nodes, c))
        .collect();
    nodes.push(node);
    id32(nodes.len() - 1)
}
