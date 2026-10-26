//! Deterministic simulation of the scan pipeline (`docs/data-pipeline.md` §10.1).
//!
//! An in-memory filesystem with injected faults is scanned by the real worker, watchdog and
//! applier code, driven in a seeded random order on one thread with virtual time. After every
//! applier step the invariants are checked; at the end every directory's totals are compared
//! with a reference computed directly from the filesystem spec.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

use dut_clean::{DeleteResult, DeleterMsg, Mode, Plan, Refusal};
use dut_engine::{Applier, Command, MeasureMsg, MeasureReq, MeasureResult, Measurement, Model};
use dut_scan::{
    Backend, Batch, Block, Budget, Clock, DirStream, FsClass, JobOptions, Limits, Raw, RawBlock,
    ScanMsg, Shared, Sink, StepResult, Worker,
};
use dut_tree::{DirState, ErrKind, Ix, Kind, Meta, NodeFlags, SEP, Subvol, Totals, Tree};
use parking_lot::Mutex;
use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

// ---- randomness ------------------------------------------------------------------------------

/// Seeded ChaCha8: the stream for a seed is fixed by the algorithm, so a failing seed replays
/// the same run on every platform.
pub struct Rng(ChaCha8Rng);

impl Rng {
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
}

// ---- filesystem spec -------------------------------------------------------------------------

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
    /// Extent map of a file on a filesystem that shares extents.
    pub pieces: Vec<SimPiece>,
    /// Measuring the file fails (it cannot be opened).
    pub unmeasurable: bool,
}

/// One extent of a simulated file (`docs/btrfs.md` §3.4). Compressed extents are identified by
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
    pub fn is_deleted(&self, i: u32) -> bool {
        self.deleted[i as usize].load(SeqCst)
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
    /// Extent maps, reflinked copies and snapshots that share data.
    pub extents: bool,
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
            extents: true,
        }
    }
}

impl Spec {
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
                mount: if cfg.mount_ids { 1 } else { 0 },
            },
            children: vec![],
            fault: Fault::None,
            vanished: false,
            subvol: None,
            dup_mount: false,
            fs_class: None,
            pieces: vec![],
            unmeasurable: false,
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
            for i in 0..ndirs {
                ino += 1;
                let roll = rng.below(1000);
                let mut fs_class = None;
                let (cdev, cmount, cino, subvol) = if cfg.other_fs && depth > 0 && roll < 30 {
                    fs_class = match rng.below(12) {
                        0 => Some(FsClass::Kernel),
                        1 | 2 => Some(FsClass::Network),
                        _ => None,
                    };
                    next_dev += 1;
                    next_mount += 1;
                    mount_dirs.push(nodes.len() as u32);
                    let fs = dev_fs.len() as u32;
                    dev_fs.insert(next_dev, (fs, cfg.extents && rng.chance(500)));
                    let m = if cfg.mount_ids { next_mount } else { 0 };
                    (next_dev, m, ino, None)
                } else if cfg.subvols && roll < 70 {
                    next_dev += 1;
                    dev_fs.insert(next_dev, dev_fs[&dev]);
                    subvol_dirs.push(nodes.len() as u32);
                    let sv = Subvol {
                        id: 256 + next_dev,
                        uuid: [next_dev as u8; 16],
                        parent_uuid: rng.chance(500).then_some([1; 16]),
                        read_only: rng.chance(333),
                        otime: 1,
                        qgroup: None,
                    };
                    (next_dev, mount, 256, Some(sv))
                } else {
                    (dev, mount, ino, None)
                };
                kids.push(nodes.len() as u32);
                nodes.push(SpecNode {
                    name: format!("d{i}").into_bytes(),
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
                    pieces: vec![],
                    unmeasurable: false,
                });
                queue.push_back((*kids.last().expect("pushed"), depth + 1));
            }
            for i in 0..nfiles {
                ino += 1;
                let size = rng.below(100_000);
                let alloc = if rng.chance(100) {
                    0
                } else {
                    size.div_ceil(4096) * 4096
                };
                kids.push(nodes.len() as u32);
                nodes.push(SpecNode {
                    name: format!("f{i}").into_bytes(),
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
                    pieces: vec![],
                    unmeasurable: cfg.faults && rng.chance(30),
                });
            }
            nodes[d as usize].children = kids;
        }
        // Hard links: some files become extra links of another file's inode (same device).
        if cfg.hardlinks {
            let files: Vec<u32> = (0..nodes.len() as u32)
                .filter(|&i| {
                    nodes[i as usize].meta.kind == Kind::File && !nodes[i as usize].vanished
                })
                .collect();
            for _ in 0..files.len() / 4 {
                let a = files[rng.below(files.len() as u64) as usize] as usize;
                let b = files[rng.below(files.len() as u64) as usize] as usize;
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
                    50..90 => Fault::EioAfter(rng.below(len as u64 + 1) as usize),
                    90..110 if len > 0 => {
                        Fault::PanicAtBlock(rng.below(last_block as u64 + 1) as usize)
                    }
                    110..120 => Fault::PanicOnOpen,
                    120..150 => Fault::Stall {
                        block: rng.below(last_block as u64 + 1) as usize,
                        dur_ns: Some(20_000_000 + rng.below(200_000_000)),
                    },
                    150..170 => Fault::Stall {
                        block: rng.below(last_block as u64 + 1) as usize,
                        dur_ns: None,
                    },
                    170..190 if len > 0 => {
                        Fault::StatPanic(rng.below(last_block as u64 + 1) as usize)
                    }
                    190..215 if len > 0 => Fault::StatStall {
                        block: rng.below(last_block as u64 + 1) as usize,
                        dur_ns: Some(20_000_000 + rng.below(200_000_000)),
                    },
                    215..230 if len > 0 => Fault::StatStall {
                        block: rng.below(last_block as u64 + 1) as usize,
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
                parent[c as usize] = i as u32;
            }
        }
        if cfg.extents {
            mark_shared(rng, &mut nodes);
        }
        let deleted = (0..nodes.len()).map(|_| AtomicBool::new(false)).collect();
        Spec {
            dev_fs,
            nodes,
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
            nodes[i].pieces = p.clone();
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
        let j = files[rng.below(files.len() as u64) as usize];
        let (fi, fj) = (dev_fs[&nodes[i].meta.dev].0, dev_fs[&nodes[j].meta.dev].0);
        if i == j || fi != fj || nodes[j].pieces.is_empty() {
            continue;
        }
        let src = nodes[j].pieces[rng.below(nodes[j].pieces.len() as u64) as usize];
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
        let k = rng.below(nodes[i].pieces.len() as u64) as usize;
        nodes[i].pieces[k] = piece;
    }
    // Snapshots: a subvolume holding a copy of another directory on the same filesystem, with
    // the same inode numbers (on the subvolume's own device) and extents.
    for &sv in subvol_dirs {
        if !rng.chance(400) {
            continue;
        }
        let dirs: Vec<u32> = (0..nodes.len() as u32)
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
        let src = dirs[rng.below(dirs.len() as u64) as usize];
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
        stack.extend(nodes[x as usize].children.clone());
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
    (nodes.len() - 1) as u32
}

// ---- backend, clock, sink --------------------------------------------------------------------

pub struct SimClock(AtomicU64);

impl SimClock {
    pub fn advance(&self, ns: u64) {
        self.0.fetch_add(ns, SeqCst);
    }
}

impl Clock for SimClock {
    fn now(&self) -> u64 {
        self.0.load(SeqCst)
    }
}

pub struct SimBackend {
    pub spec: Arc<Spec>,
    pub clock: Arc<SimClock>,
}

pub struct SimStream {
    spec: Arc<Spec>,
    clock: Arc<SimClock>,
    node: u32,
    /// Live children when the directory was opened.
    children: Vec<u32>,
    pos: usize,
    stall_start: u64,
    stall_done: bool,
    eio_pending: bool,
}

impl DirStream for SimStream {
    type Loc = u32;

    fn next_raw(&mut self, max_entries: usize, _max_bytes: usize) -> io::Result<(Raw<u32>, bool)> {
        let n = &self.spec.nodes[self.node as usize];
        let block_idx = self.pos / max_entries;
        if self.eio_pending {
            return Err(io::Error::from_raw_os_error(5));
        }
        match n.fault {
            Fault::Stall { block, dur_ns } if block == block_idx && !self.stall_done => {
                let now = self.clock.now();
                if self.stall_start == 0 {
                    self.stall_start = now;
                }
                match dur_ns {
                    Some(d) if now >= self.stall_start + d => self.stall_done = true,
                    _ => return Err(io::ErrorKind::WouldBlock.into()),
                }
            }
            Fault::PanicAtBlock(b) if b == block_idx => panic!("injected panic"),
            _ => {}
        }
        let len = self.children.len();
        let limit = match n.fault {
            Fault::EioAfter(k) => k.min(len),
            _ => len,
        };
        if self.pos >= limit && limit < len {
            return Err(io::Error::from_raw_os_error(5));
        }
        let end = (self.pos + max_entries).min(limit);
        let raw = SimRaw {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: self.node,
            entries: self.children[self.pos..end].to_vec(),
            block: block_idx,
            stall_start: 0,
            stall_done: false,
        };
        self.pos = end;
        if self.pos >= limit && limit < len {
            self.eio_pending = true;
            return Ok((Box::new(raw), true));
        }
        Ok((Box::new(raw), self.pos < len))
    }
}

/// A block of a simulated directory, with the directory's stat-phase faults.
pub struct SimRaw {
    spec: Arc<Spec>,
    clock: Arc<SimClock>,
    node: u32,
    entries: Vec<u32>,
    block: usize,
    stall_start: u64,
    stall_done: bool,
}

impl RawBlock<u32> for SimRaw {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stat(&mut self, out: &mut Block<u32>) -> io::Result<()> {
        match self.spec.nodes[self.node as usize].fault {
            Fault::StatStall { block, dur_ns } if block == self.block && !self.stall_done => {
                let now = self.clock.now();
                if self.stall_start == 0 {
                    self.stall_start = now;
                }
                match dur_ns {
                    Some(d) if now >= self.stall_start + d => self.stall_done = true,
                    _ => return Err(io::ErrorKind::WouldBlock.into()),
                }
            }
            Fault::StatPanic(b) if b == self.block => panic!("injected panic"),
            _ => {}
        }
        for &c in &self.entries {
            let cn = &self.spec.nodes[c as usize];
            let meta = if cn.vanished {
                Err(ErrKind::NotFound)
            } else {
                Ok(cn.meta)
            };
            let child = (cn.meta.kind == Kind::Dir).then_some(c);
            out.push(&cn.name, meta, child);
        }
        self.entries.clear();
        Ok(())
    }
}

impl Backend for SimBackend {
    type Loc = u32;
    type Stream = SimStream;

    fn loc_from_path(&self, path: &Path) -> u32 {
        // Paths below the root are "/sim/d0/d1"; resolve them by name.
        let s = path.to_string_lossy();
        let rest = s.strip_prefix(ROOT_NAME).unwrap_or("");
        let mut cur = ROOT;
        for part in rest.split(['/', '\\']).filter(|p| !p.is_empty()) {
            let n = &self.spec.nodes[cur as usize];
            cur = *n
                .children
                .iter()
                .find(|&&c| self.spec.nodes[c as usize].name == part.as_bytes())
                .expect("path in spec");
        }
        cur
    }

    fn stat_root(&self, loc: &u32) -> io::Result<Meta> {
        Ok(self.spec.nodes[*loc as usize].meta)
    }

    fn mount_class(&self, mount: u64, dev: u64) -> Option<FsClass> {
        let n = self.spec.nodes.iter().find(|n| {
            n.meta.kind == Kind::Dir
                && if mount != 0 {
                    n.meta.mount == mount
                } else {
                    n.meta.dev == dev
                }
        })?;
        Some(n.fs_class.unwrap_or(FsClass::Local))
    }

    fn duplicate_mounts(&self, _scope: &Path) -> Option<HashSet<u64>> {
        let mut dups = HashSet::new();
        for n in &self.spec.nodes {
            if n.dup_mount {
                if n.meta.mount == 0 {
                    // No mount ids: no mount table either.
                    return None;
                }
                dups.insert(n.meta.mount);
            }
        }
        Some(dups)
    }

    fn shares_extents(&self, loc: &u32) -> bool {
        let dev = self.spec.nodes[*loc as usize].meta.dev;
        self.spec.dev_fs.get(&dev).is_some_and(|d| d.1)
    }

    fn subvolume(&self, loc: &u32) -> io::Result<Option<Subvol>> {
        Ok(self.spec.nodes[*loc as usize].subvol.clone())
    }

    fn open(&self, loc: &u32) -> io::Result<SimStream> {
        if self.spec.is_deleted(*loc) {
            return Err(io::ErrorKind::NotFound.into());
        }
        match self.spec.nodes[*loc as usize].fault {
            Fault::Eacces => return Err(io::ErrorKind::PermissionDenied.into()),
            Fault::PanicOnOpen => panic!("injected panic"),
            _ => {}
        }
        Ok(SimStream {
            spec: self.spec.clone(),
            clock: self.clock.clone(),
            node: *loc,
            children: self.spec.live_children(*loc).collect(),
            pos: 0,
            stall_start: 0,
            stall_done: false,
            eio_pending: false,
        })
    }
}

impl SimBackend {
    /// What the simulated measurer answers: the spec's extent map, unless the file is gone or
    /// changed, or cannot be measured.
    pub fn measure(&self, req: &MeasureReq) -> MeasureMsg {
        use dut_scan::Backend as _;
        let file = &req.files[0];
        let i = self.loc_from_path(&req.path(file));
        let n = &self.spec.nodes[i as usize];
        let result = if self.spec.is_deleted(i)
            || n.vanished
            || (n.meta.dev, n.meta.ino) != file.inode
            || n.meta.kind != Kind::File
        {
            MeasureResult::Changed
        } else if n.unmeasurable {
            MeasureResult::Failed(ErrKind::PermissionDenied)
        } else {
            let fs = self.spec.dev_fs[&n.meta.dev].0;
            let mut m = Measurement {
                fs: [fs as u8; 16],
                ..Default::default()
            };
            for p in &n.pieces {
                m.compressed |= p.encoded;
                match (p.shared, p.encoded) {
                    (false, _) => m.excl += p.len,
                    (true, true) => m.points.push((p.phys, p.len)),
                    (true, false) => m.ranges.push((p.phys, p.phys + p.len)),
                }
            }
            MeasureResult::Measured(m)
        };
        MeasureMsg {
            round: req.round,
            node: file.node,
            inode: file.inode,
            result,
        }
    }
}

pub struct SimSink {
    q: Mutex<VecDeque<Batch>>,
    pub budget: Budget,
    pub max_used: AtomicU64,
}

impl Sink for SimSink {
    fn try_send(&self, mut b: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(b.bytes) {
            Some(n) => {
                b.bytes = n;
                self.q.lock().push_back(b);
                self.max_used.fetch_max(self.budget.used() as u64, SeqCst);
                Ok(())
            }
            None => Err(b),
        }
    }
    fn send(&self, _b: Batch) {
        unreachable!("simulated workers never block");
    }
    fn send_control(&self, m: ScanMsg) {
        self.q.lock().push_back(Batch {
            msgs: vec![m],
            bytes: 0,
        });
    }
}

// ---- driver ----------------------------------------------------------------------------------

pub struct SimConfig {
    pub workers: usize,
    pub budget: usize,
    pub limits: Limits,
    pub auto_abandon_ns: u64,
    pub rescans: u32,
    /// Delete commands issued at random points; results are chosen by the simulated deleter.
    pub deletes: u32,
    /// Cancel commands for random running jobs.
    pub cancels: u32,
    pub max_ticks: u64,
    pub spec: SpecConfig,
    /// Scan with `cross_mounts`; `None` picks it per seed.
    pub cross_mounts: Option<bool>,
    /// Scan with `network`; `None` picks it per seed.
    pub network: Option<bool>,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            workers: 3,
            budget: 16 * 1024,
            limits: Limits {
                block_entries: 3,
                block_name_bytes: 1 << 20,
                outbox_bytes: 4 * 1024,
                outbox_msgs: 5,
                flush_age_ns: 2_000_000,
                stall_ns: 10_000_000,
                chunk_credit: 2,
            },
            auto_abandon_ns: 400_000_000,
            rescans: 3,
            deletes: 3,
            cancels: 1,
            max_ticks: 2_000_000,
            spec: SpecConfig::default(),
            cross_mounts: None,
            network: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct SimReport {
    pub ticks: u64,
    pub dirs: usize,
    pub rescans: u32,
    pub deletes: u32,
    pub deleted_items: u64,
    pub cancels: u32,
    pub recoveries: u32,
    pub abandoned: u64,
    pub max_workers: usize,
    pub max_budget_used: u64,
    pub stalled_msgs: u64,
    pub late: u64,
    pub discarded_cut: u64,
    /// Subvolume records, read-only ones among them, and duplicate mounts in the final tree.
    pub subvols: u64,
    pub ro_subvols: u64,
    pub dup_mounts: u64,
    /// Runs with `cross_mounts`.
    pub cross_runs: u64,
    /// Network and kernel mounts left unscanned in the final tree.
    pub network_skipped: u64,
    pub kernel_skipped: u64,
    /// Measurements answered, and bytes counted once instead of per file in the final tree.
    pub measured: u64,
    pub shared_bytes: u64,
    /// Blocks stat'ed by chunk tasks rather than by their directory's listing task.
    pub chunk_tasks: u64,
}

pub fn run(seed: u64, cfg: &SimConfig) -> Result<SimReport, String> {
    let mut rng = Rng::new(seed);
    let spec = Arc::new(Spec::generate(
        &mut rng,
        &cfg.spec,
        cfg.limits.block_entries,
    ));
    let cross_mounts = cfg.cross_mounts.unwrap_or_else(|| rng.chance(400));
    let network = cfg.network.unwrap_or_else(|| rng.chance(300));
    let clock = Arc::new(SimClock(AtomicU64::new(1)));
    let backend = SimBackend {
        spec: spec.clone(),
        clock: clock.clone(),
    };
    let shared = Arc::new(Shared::new(backend, cfg.limits.clone()));
    let sink = Arc::new(SimSink {
        q: Mutex::new(VecDeque::new()),
        budget: Budget::new(cfg.budget),
        max_used: AtomicU64::new(0),
    });
    let mut workers: Vec<Worker<SimBackend>> = (0..cfg.workers)
        .map(|_| Worker::new(&shared, false))
        .collect();
    let mut applier = Applier::new(shared.clone());
    let mut model = Model::default();
    let mut cur: Option<(VecDeque<ScanMsg>, usize)> = None;
    let mut rescans_left = cfg.rescans;
    let mut deletes_left = cfg.deletes;
    let mut cancels_left = cfg.cancels;
    let mut recoveries = 0;
    let mut plans: VecDeque<(Plan, usize)> = VecDeque::new();
    let mut del_q: VecDeque<DeleterMsg> = VecDeque::new();
    let mut meas_q: Vec<MeasureReq> = Vec::new();
    let mut report = SimReport::default();

    applier.command(
        &mut model,
        Command::Scan {
            path: PathBuf::from(ROOT_NAME),
            opts: JobOptions {
                cross_mounts,
                network,
                ..JobOptions::default()
            },
        },
        clock.now(),
    );
    let fail = |what: String, report: &SimReport| {
        Err(format!(
            "seed {seed}: {what} (after {} ticks)",
            report.ticks
        ))
    };

    loop {
        report.ticks += 1;
        if report.ticks > cfg.max_ticks {
            let (out, ab) = shared.outstanding();
            eprintln!("{}", shared.debug_parts());
            return fail(
                format!(
                    "did not finish: running jobs {}, outstanding {out}, abandoned {ab}, queued batches {}",
                    model.status.running(),
                    sink.q.lock().len()
                ),
                &report,
            );
        }
        // One request per file, so answers can arrive in any order.
        for r in applier.take_measure_requests() {
            for f in r.files {
                meas_q.push(MeasureReq {
                    round: r.round,
                    dir: r.dir.clone(),
                    files: vec![f],
                });
            }
        }
        let r = rng.below(100);
        match r {
            0..55 => {
                let i = rng.below(workers.len() as u64) as usize;
                let _: StepResult = workers[i].step(&shared, &*sink, &*clock);
            }
            55..60 => {
                // Answers in any order, interleaved with everything else.
                for _ in 0..=rng.below(8) {
                    if meas_q.is_empty() {
                        break;
                    }
                    let req = meas_q.swap_remove(rng.below(meas_q.len() as u64) as usize);
                    let msg = shared.backend.measure(&req);
                    report.measured += matches!(msg.result, MeasureResult::Measured(_)) as u64;
                    applier.apply_measure(&mut model, msg, clock.now());
                    if let Err(e) = applier.check(&model) {
                        return fail(format!("invariant after measurement: {e}"), &report);
                    }
                }
            }
            60..80 => {
                for _ in 0..=rng.below(20) {
                    if let Some(d) = del_q.pop_front() {
                        if let (Some(tr), DeleterMsg::Item { node, result, .. }) = (trace(), &d) {
                            match model.tree.resolve(*node) {
                                Some(ix) => {
                                    let p =
                                        String::from_utf8_lossy(&model.tree.path(ix)).into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] result {p}: {result:?}", report.ticks);
                                    }
                                }
                                None => eprintln!(
                                    "[{}] result for unresolvable node: {result:?}",
                                    report.ticks
                                ),
                            }
                        }
                        applier.apply_delete(&mut model, d);
                    } else if applier.has_work() {
                        applier.work_step(&mut model);
                    } else {
                        if cur.is_none() {
                            match sink.q.lock().pop_front() {
                                Some(b) => cur = Some((b.msgs.into(), b.bytes)),
                                None => break,
                            }
                        }
                        let (msgs, bytes) = cur.as_mut().expect("batch");
                        match msgs.pop_front() {
                            Some(m) => applier.apply(&mut model, m, clock.now()),
                            None => {
                                sink.budget.release(*bytes);
                                cur = None;
                            }
                        }
                    }
                    applier.refresh_status(&mut model);
                    if let Err(e) = applier.check(&model) {
                        return fail(format!("invariant: {e}"), &report);
                    }
                }
                plans.extend(applier.take_plans().into_iter().map(|p| (p, 0)));
            }
            80..90 => {
                let rep = shared.watchdog_round(&*sink, clock.now());
                // Same replacement policy as the real runtime. Simulated workers never exit, so
                // the pool only grows here; that is the worst case for the bound.
                if workers.len() < cfg.workers + shared.allowed_extra_workers(cfg.workers, &rep) {
                    workers.push(Worker::new(&shared, false));
                }
                report.max_workers = report.max_workers.max(workers.len());
                clock.advance(1_000_000);
            }
            90..93 => applier.tick(&mut model, clock.now(), Some(cfg.auto_abandon_ns)),
            93..96 => {
                if let Some(msg) = deleter_step(&spec, &shared, &mut plans, &mut rng, &mut report) {
                    del_q.push_back(msg);
                }
            }
            _ => {
                clock.advance(rng.below(3) * 1_000_000);
                if cancels_left > 0 && rng.chance(8) {
                    let running: Vec<_> = model
                        .status
                        .jobs
                        .iter()
                        .filter(|j| j.phase == dut_engine::JobPhase::Running)
                        .map(|j| j.id)
                        .collect();
                    if !running.is_empty() {
                        let job = running[rng.below(running.len() as u64) as usize];
                        cancels_left -= 1;
                        report.cancels += 1;
                        applier.command(&mut model, Command::Cancel { job }, clock.now());
                        if let Err(e) = applier.check(&model) {
                            return fail(format!("invariant after cancel: {e}"), &report);
                        }
                    }
                }
                if deletes_left > 0 && rng.chance(15) {
                    let items = pick_deletable(&model.tree, &spec, &shared, &mut rng);
                    if !items.is_empty() {
                        deletes_left -= 1;
                        report.deletes += 1;
                        if let Some(tr) = trace() {
                            for r in &items {
                                if let Some(ix) = model.tree.resolve(*r) {
                                    let p =
                                        String::from_utf8_lossy(&model.tree.path(ix)).into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] delete submitted {p}", report.ticks);
                                    }
                                }
                            }
                        }
                        applier.command(
                            &mut model,
                            Command::Delete {
                                items,
                                mode: Mode::Permanent,
                            },
                            clock.now(),
                        );
                        plans.extend(applier.take_plans().into_iter().map(|p| (p, 0)));
                        if let Err(e) = applier.check(&model) {
                            return fail(format!("invariant after delete: {e}"), &report);
                        }
                    }
                }
                if rescans_left > 0
                    && rng.chance(20)
                    && let Some(node) = random_dir(&model.tree, &mut rng)
                {
                    rescans_left -= 1;
                    report.rescans += 1;
                    let r = model.tree.node_ref(node);
                    if let Some(tr) = trace() {
                        let p = String::from_utf8_lossy(&model.tree.path(node)).into_owned();
                        if tr.starts_with(&p) {
                            eprintln!("[{}] rescan requested {p}", report.ticks);
                        }
                    }
                    applier.command(&mut model, Command::Rescan { node: r }, clock.now());
                    if let Err(e) = applier.check(&model) {
                        return fail(format!("invariant after rescan: {e}"), &report);
                    }
                }
            }
        }
        let (out, ab) = shared.outstanding();
        if out == ab
            && model.status.running() == 0
            && !applier.has_work()
            && cur.is_none()
            && sink.q.lock().is_empty()
            && model.status.commands_applied > 0
            && plans.is_empty()
            && del_q.is_empty()
            && model.status.deleting.is_none()
            && meas_q.is_empty()
            && model.status.measuring.is_none()
        {
            // A cancelled job leaves directories incomplete (Cancelled, or Superseded when a
            // cancelled rescan had cut them). Rescan the topmost one, as a user would, until
            // none remain; then the tree must match the filesystem again.
            match topmost_incomplete(&model.tree) {
                Some(ix) if recoveries < 100 => {
                    recoveries += 1;
                    report.recoveries += 1;
                    let r = model.tree.node_ref(ix);
                    applier.command(&mut model, Command::Rescan { node: r }, clock.now());
                }
                Some(_) => {
                    return fail(
                        "incomplete directories remain after 100 rescans".into(),
                        &report,
                    );
                }
                None => break,
            }
        }
    }

    report.chunk_tasks = shared.chunk_tasks.load(SeqCst);
    report.subvols = model.subvols.len() as u64;
    report.ro_subvols = model.subvols.values().filter(|s| s.read_only).count() as u64;
    report.cross_runs = cross_mounts as u64;
    if let Some(root) = model.tree.roots().next() {
        let t = model.tree.total(root);
        report.shared_bytes = t.referenced.saturating_sub(t.alloc);
    }
    if let Some(root) = model.tree.roots().next() {
        let mut stack = vec![root];
        while let Some(ix) = stack.pop() {
            let f = model.tree.flags(ix);
            report.dup_mounts += f.has(NodeFlags::DUP_MOUNT) as u64;
            report.network_skipped += f.has(NodeFlags::NETWORK) as u64;
            report.kernel_skipped += f.has(NodeFlags::KERNEL) as u64;
            stack.extend(model.tree.children(ix));
        }
    }
    let st = &model.status.stats;
    report.max_budget_used = sink.max_used.load(SeqCst);
    report.stalled_msgs = st.stalled;
    report.late = st.late;
    report.discarded_cut = st.discarded_cut;
    if st.corrupt + st.lost + st.duplicate > 0 {
        return fail(
            format!(
                "corrupt {} lost {} duplicate {}: {:?}",
                st.corrupt,
                st.lost,
                st.duplicate,
                &model.status.anomalies[..model.status.anomalies.len().min(5)]
            ),
            &report,
        );
    }
    if report.max_budget_used > cfg.budget as u64 {
        return fail(
            format!("budget exceeded: {}", report.max_budget_used),
            &report,
        );
    }
    applier
        .check(&model)
        .map_err(|e| format!("seed {seed}: final invariant: {e}"))?;
    applier
        .check_settled(&model)
        .map_err(|e| format!("seed {seed}: settled: {e}"))?;
    if let Ok(v) = std::env::var("DUT_SIM_INODE") {
        let mut it = v
            .split(',')
            .map(|x| x.parse::<u64>().expect("DUT_SIM_INODE=dev,ino"));
        let key = (it.next().expect("dev"), it.next().expect("ino"));
        eprintln!("{}", applier.describe_extents(&model, key));
        for n in spec
            .nodes
            .iter()
            .filter(|n| (n.meta.dev, n.meta.ino) == key)
        {
            eprintln!("  spec pieces {:?}", n.pieces);
        }
    }
    let (dirs, abandoned) = compare(
        &spec,
        &model,
        cfg.limits.block_entries,
        cross_mounts,
        network,
    )
    .map_err(|e| {
        let n = &model.status.notices;
        format!(
            "seed {seed}: {e}; last notices: {:?}",
            &n[n.len().saturating_sub(6)..]
        )
    })?;
    report.dirs = dirs;
    report.abandoned = abandoned;
    Ok(report)
}

fn trace() -> Option<String> {
    std::env::var("DUT_SIM_TRACE").ok()
}

fn spec_node(shared: &Shared<SimBackend>, tree: &Tree, ix: Ix) -> u32 {
    use dut_scan::Backend as _;
    shared.backend.loc_from_path(&tree.path_buf(ix))
}

/// One to three visible entries whose removal cannot shift a fault position.
fn pick_deletable(
    tree: &Tree,
    spec: &Spec,
    shared: &Shared<SimBackend>,
    rng: &mut Rng,
) -> Vec<dut_tree::NodeRef> {
    let Some(root) = tree.roots().next() else {
        return vec![];
    };
    let mut all = vec![];
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        stack.extend(tree.children(ix));
        if ix != root
            && !tree.flags(ix).has(NodeFlags::DELETING)
            && spec.deletable(spec_node(shared, tree, ix))
        {
            all.push(ix);
        }
    }
    let mut out = vec![];
    for _ in 0..=rng.below(3) {
        if all.is_empty() {
            break;
        }
        let i = rng.below(all.len() as u64) as usize;
        out.push(tree.node_ref(all.swap_remove(i)));
    }
    out
}

/// The simulated deleter: handles one item of the oldest plan, choosing a result and applying
/// it to the spec, or reports the plan done.
fn deleter_step(
    spec: &Spec,
    shared: &Shared<SimBackend>,
    plans: &mut VecDeque<(Plan, usize)>,
    rng: &mut Rng,
    report: &mut SimReport,
) -> Option<DeleterMsg> {
    use dut_scan::Backend as _;
    let (plan, i) = plans.front_mut()?;
    if *i >= plan.items.len() {
        let id = plan.id;
        plans.pop_front();
        return Some(DeleterMsg::Done {
            plan: id,
            btrfs: false,
        });
    }
    let item = plan.items[*i].clone();
    *i += 1;
    let n = shared.backend.loc_from_path(&item.path);
    let is_dir = spec.nodes[n as usize].meta.kind == Kind::Dir;
    let result = match rng.below(100) {
        0..70 => {
            spec.delete_subtree(n);
            report.deleted_items += 1;
            DeleteResult::Deleted
        }
        70..78 => DeleteResult::Stale("changed".into()),
        78..82 => DeleteResult::Refused(Refusal::Protected),
        82..86 => DeleteResult::Aborted,
        86..92 => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
        _ if is_dir => {
            // Part of the tree went before the error.
            let mut stack: Vec<u32> = spec.live_children(n).collect();
            while let Some(d) = stack.pop() {
                stack.extend(spec.live_children(d));
                if spec.deletable(d) && rng.chance(500) {
                    spec.delete_subtree(d);
                }
            }
            DeleteResult::Failed {
                partial: true,
                err: ErrKind::Other,
                msg: "eio part-way".into(),
            }
        }
        _ => DeleteResult::Failed {
            partial: false,
            err: ErrKind::Other,
            msg: "eio".into(),
        },
    };
    Some(DeleterMsg::Item {
        plan: plan.id,
        node: item.node,
        bytes: item.bytes,
        result,
    })
}

/// The visible directory closest to the root that a cancelled job left incomplete.
fn topmost_incomplete(tree: &Tree) -> Option<Ix> {
    let root = tree.roots().next()?;
    let mut queue = VecDeque::from([root]);
    while let Some(ix) = queue.pop_front() {
        let d = tree.dir(ix)?;
        if d.cut || matches!(d.state, DirState::Superseded | DirState::Cancelled) {
            return Some(ix);
        }
        queue.extend(tree.children(ix).filter(|&c| tree.dir(c).is_some()));
    }
    None
}

fn random_dir(tree: &Tree, rng: &mut Rng) -> Option<Ix> {
    let root = tree.roots().next()?;
    let mut dirs = vec![];
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        dirs.push(d);
        stack.extend(tree.children(d).filter(|&c| tree.dir(c).is_some()));
    }
    dirs.get(rng.below(dirs.len() as u64) as usize).copied()
}

// ---- reference -------------------------------------------------------------------------------

/// Shared-extent accounting by brute force (`docs/btrfs.md` §3.4): which files are measured
/// (with their unshared bytes), and the shared bytes counted at each path, with the part shared
/// only outside the scan.
#[allow(clippy::type_complexity)]
fn expected_homes(
    spec: &Spec,
    seen: &[(u32, Vec<u8>)],
    charged: &HashMap<(u64, u64), Vec<u8>>,
) -> (HashMap<Vec<u8>, u64>, HashMap<Vec<u8>, (u64, u64)>) {
    let mut measured: HashMap<Vec<u8>, u64> = HashMap::new();
    let mut ranges: HashMap<u32, Vec<(u64, u64, &Vec<u8>)>> = HashMap::new();
    let mut points: HashMap<(u32, u64), (u64, Vec<&Vec<u8>>)> = HashMap::new();
    for (d, path) in seen {
        let n = &spec.nodes[*d as usize];
        let Some(&(fs, capable)) = spec.dev_fs.get(&n.meta.dev) else {
            continue;
        };
        if n.meta.kind != Kind::File
            || n.vanished
            || n.meta.alloc == 0
            || n.unmeasurable
            || !capable
            || (n.meta.nlink > 1 && charged.get(&(n.meta.dev, n.meta.ino)) != Some(path))
        {
            continue;
        }
        let mut excl = 0;
        for p in &n.pieces {
            match (p.shared, p.encoded) {
                (false, _) => excl += p.len,
                (true, true) => {
                    let e = points.entry((fs, p.phys)).or_default();
                    e.0 = e.0.max(p.len);
                    if !e.1.contains(&path) {
                        e.1.push(path);
                    }
                }
                (true, false) => ranges
                    .entry(fs)
                    .or_default()
                    .push((p.phys, p.phys + p.len, path)),
            }
        }
        measured.insert(path.clone(), excl);
    }
    let lca = |paths: &[&Vec<u8>]| -> Vec<u8> {
        let mut l = paths[0].clone();
        for p in &paths[1..] {
            while !(p.starts_with(&l) && (p.len() == l.len() || p[l.len()] == SEP)) {
                let i = l.iter().rposition(|&c| c == SEP).expect("below the root");
                l.truncate(i);
            }
        }
        l
    };
    let mut homes: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
    for rs in ranges.values() {
        let mut bounds: Vec<u64> = rs.iter().flat_map(|r| [r.0, r.1]).collect();
        bounds.sort();
        bounds.dedup();
        for w in bounds.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut refs: Vec<&Vec<u8>> = rs
                .iter()
                .filter(|r| r.0 <= a && b <= r.1)
                .map(|r| r.2)
                .collect();
            refs.dedup();
            refs.sort();
            refs.dedup();
            if refs.is_empty() {
                continue;
            }
            let e = homes.entry(lca(&refs)).or_default();
            e.0 += b - a;
            if refs.len() == 1 {
                e.1 += b - a;
            }
        }
    }
    for (len, refs) in points.values() {
        let e = homes.entry(lca(refs)).or_default();
        e.0 += len;
        if refs.len() == 1 {
            e.1 += len;
        }
    }
    (measured, homes)
}

/// A directory's subvolume flags and record agree with the spec. A subvolume that was read
/// completely must have its record; one that failed to open may lack it.
fn check_subvol(spec: &Spec, model: &Model, d: u32, ix: Ix, path: &[u8]) -> Result<(), String> {
    let tree = &model.tree;
    let flags = tree.flags(ix);
    let rec = model.subvols.get(&tree.node_ref(ix));
    let want = &spec.nodes[d as usize].subvol;
    let p = String::from_utf8_lossy(path);
    match (want, rec) {
        (Some(w), Some(r)) if w == r => {}
        (None, None) => {}
        (Some(_), None) if tree.dir(ix).is_some_and(|r| r.state != DirState::Complete) => {}
        (w, r) => return Err(format!("{p}: subvolume record {r:?}, expected {w:?}")),
    }
    let ro = rec.is_some_and(|r| r.read_only);
    if flags.has(NodeFlags::SUBVOL) != rec.is_some() || flags.has(NodeFlags::RO_SUBVOL) != ro {
        return Err(format!("{p}: subvolume flags {flags:?} for record {rec:?}"));
    }
    Ok(())
}

fn allowed(fault: &Fault, len: usize, block: usize, state: DirState) -> bool {
    use DirState::*;
    // A stat-phase fault in a block past the end is never reached.
    let exists = |b: usize| b * block < len;
    match fault {
        Fault::None => state == Complete,
        // An error after the last entry is never reached.
        Fault::EioAfter(k) if *k >= len => state == Complete,
        Fault::Eacces => state == Failed,
        Fault::EioAfter(0) => state == Failed,
        Fault::EioAfter(_) => state == Partial,
        Fault::PanicAtBlock(_) | Fault::PanicOnOpen => state == Partial,
        Fault::Stall { dur_ns: None, .. } => state == Abandoned,
        Fault::Stall {
            dur_ns: Some(_), ..
        } => matches!(state, Complete | Abandoned),
        Fault::StatPanic(b) if exists(*b) => state == Partial,
        Fault::StatStall {
            block: b,
            dur_ns: None,
        } if exists(*b) => state == Abandoned,
        Fault::StatStall { block: b, .. } if exists(*b) => matches!(state, Complete | Abandoned),
        Fault::StatPanic(_) | Fault::StatStall { .. } => state == Complete,
    }
}

/// Entries of a directory the scan can have seen (indexes into its live children), given its
/// fault and final state. `len` is the number of live children (faulted directories never
/// lose children, see `Spec::deletable`). An abandoned directory keeps the blocks that arrived
/// before it was abandoned, which depends on timing: those are read from the tree (`present`),
/// and each must be whole and before the stuck one.
fn seen_children(
    n: &SpecNode,
    len: usize,
    state: DirState,
    block: usize,
    present: impl Fn(usize) -> bool,
) -> Result<Vec<usize>, String> {
    let all = 0..len;
    Ok(match (state, &n.fault) {
        (DirState::Complete, _) => all.collect(),
        (DirState::Partial, Fault::EioAfter(k)) => (0..(*k).min(len)).collect(),
        (DirState::Partial, Fault::PanicAtBlock(b)) => (0..(b * block).min(len)).collect(),
        (DirState::Partial, Fault::StatPanic(b)) => all.filter(|i| i / block != *b).collect(),
        (DirState::Abandoned, f) => {
            // With a stall that never ends, blocks from the stuck one on (reading) or the stuck
            // one (stat'ing) cannot have arrived. After a stall that ends, any block can have:
            // the directory stays marked stalled until its Close, and is abandoned on time.
            let barred = |k: usize| match f {
                Fault::Stall {
                    block: b,
                    dur_ns: None,
                } => k >= *b,
                Fault::StatStall {
                    block: b,
                    dur_ns: None,
                } => k == *b,
                _ => false,
            };
            let mut out = vec![];
            for k in 0..len.div_ceil(block) {
                let idx: Vec<usize> = (k * block..((k + 1) * block).min(len)).collect();
                let here = idx.iter().filter(|&&i| present(i)).count();
                if here == 0 {
                    continue;
                }
                if here != idx.len() || barred(k) {
                    return Err(format!(
                        "abandoned with {here} of {} entries of block {k} ({f:?})",
                        idx.len()
                    ));
                }
                out.extend(idx);
            }
            out
        }
        _ => vec![],
    })
}

/// Compares every directory in the final tree with the spec. Returns (dirs checked, abandoned).
fn compare(
    spec: &Spec,
    model: &Model,
    block: usize,
    cross: bool,
    network: bool,
) -> Result<(usize, u64), String> {
    let tree = &model.tree;
    let root = tree.roots().next().ok_or("no root")?;
    // Final state of every directory, by path.
    let mut by_path: HashMap<Vec<u8>, Ix> = HashMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        by_path.insert(tree.path(ix), ix);
        stack.extend(tree.children(ix));
    }
    let state_of = |p: &[u8]| by_path.get(p).and_then(|&ix| tree.dir(ix)).map(|d| d.state);

    // Which spec entries the scan saw, and the lowest path of every hard-linked inode.
    let mount_root = |d: u32| {
        let (n, p) = (
            &spec.nodes[d as usize].meta,
            &spec.nodes[spec.parent[d as usize] as usize].meta,
        );
        d != ROOT
            && if n.mount != 0 && p.mount != 0 {
                n.mount != p.mount
            } else {
                n.dev != p.dev
            }
    };
    // With cross_mounts, how many times each mounted directory was descended into.
    let mut descended: HashMap<(u64, u64), u32> = HashMap::new();
    let mut seen: Vec<(u32, Vec<u8>)> = vec![];
    let mut charged: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
    let mut abandoned = 0;
    let mut stack = vec![(ROOT, ROOT_NAME.as_bytes().to_vec())];
    while let Some((d, path)) = stack.pop() {
        let n = &spec.nodes[d as usize];
        seen.push((d, path.clone()));
        if n.meta.kind != Kind::Dir {
            if !n.vanished && n.meta.nlink > 1 {
                let e = charged
                    .entry((n.meta.dev, n.meta.ino))
                    .or_insert_with(|| path.clone());
                if path < *e {
                    *e = path.clone();
                }
            }
            continue;
        }
        let ix = *by_path
            .get(&path)
            .ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&path)))?;
        let flags = tree.flags(ix);
        if flags.has(NodeFlags::MOUNT_ROOT) != mount_root(d) {
            return Err(format!(
                "{}: mount root flag {} for a {}",
                String::from_utf8_lossy(&path),
                flags.has(NodeFlags::MOUNT_ROOT),
                if mount_root(d) { "mount" } else { "directory" }
            ));
        }
        check_subvol(spec, model, d, ix, &path)?;
        // With mount ids the duplicates are known; without, the first mount seen wins.
        let dup = if n.meta.mount != 0 {
            n.dup_mount
        } else {
            flags.has(NodeFlags::DUP_MOUNT)
        };
        // Kernel filesystems are never descended into, network ones only with `network`; the
        // class is known only for mount roots the backend can name.
        let class = if mount_root(d) { n.fs_class } else { None };
        let wanted = match class {
            Some(FsClass::Kernel) => false,
            Some(FsClass::Network) => network,
            _ => cross,
        };
        for (flag, c, what) in [
            (NodeFlags::NETWORK, FsClass::Network, "network"),
            (NodeFlags::KERNEL, FsClass::Kernel, "kernel"),
        ] {
            if flags.has(flag) != (class == Some(c) && !wanted) {
                return Err(format!(
                    "{}: {what} flag {} for class {class:?}",
                    String::from_utf8_lossy(&path),
                    flags.has(flag)
                ));
            }
        }
        if wanted && mount_root(d) && flags.has(NodeFlags::DUP_MOUNT) != dup {
            return Err(format!(
                "{}: duplicate-mount flag {} expected {dup}",
                String::from_utf8_lossy(&path),
                flags.has(NodeFlags::DUP_MOUNT)
            ));
        }
        if mount_root(d) {
            if !wanted || dup {
                if state_of(&path) != Some(DirState::Skipped) {
                    return Err(format!(
                        "{}: mount not skipped",
                        String::from_utf8_lossy(&path)
                    ));
                }
                continue;
            }
            *descended.entry((n.meta.dev, n.meta.ino)).or_default() += 1;
        } else if flags.has(NodeFlags::DUP_MOUNT) {
            return Err(format!(
                "{}: marked as a duplicate mount",
                String::from_utf8_lossy(&path)
            ));
        }
        let state =
            state_of(&path).ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&path)))?;
        let fault = if d == ROOT { &Fault::None } else { &n.fault };
        if !allowed(fault, n.children.len(), block, state) {
            return Err(format!(
                "{}: state {state:?} not allowed for {fault:?}",
                String::from_utf8_lossy(&path)
            ));
        }
        abandoned += (state == DirState::Abandoned) as u64;
        let live: Vec<u32> = spec.live_children(d).collect();
        let child_path = |c: u32| {
            let mut p = path.clone();
            p.push(SEP);
            p.extend_from_slice(&spec.nodes[c as usize].name);
            p
        };
        let picked = seen_children(n, live.len(), state, block, |i| {
            by_path.contains_key(&child_path(live[i]))
        })
        .map_err(|e| format!("{}: {e}", String::from_utf8_lossy(&path)))?;
        for i in picked {
            stack.push((live[i], child_path(live[i])));
        }
    }

    if let Some((k, n)) = descended.iter().find(|&(_, &n)| n > 1) {
        return Err(format!("mount {k:?} descended into {n} times"));
    }

    let (measured, homes) = expected_homes(spec, &seen, &charged);

    // Expected totals bottom-up.
    let mut expect: HashMap<Vec<u8>, Totals> = HashMap::new();
    seen.sort_by_key(|s| std::cmp::Reverse(s.1.len()));
    for (d, path) in &seen {
        let n = &spec.nodes[*d as usize];
        let mut t = if n.meta.kind == Kind::Dir || !n.vanished {
            let counted = n.meta.kind == Kind::Dir
                || n.meta.nlink <= 1
                || charged.get(&(n.meta.dev, n.meta.ino)) == Some(path);
            let alloc = match measured.get(path) {
                Some(&excl) => excl,
                None => n.meta.alloc,
            };
            Totals {
                apparent: if counted { n.meta.apparent } else { 0 },
                alloc: if counted { alloc } else { 0 },
                referenced: if counted { n.meta.alloc } else { 0 },
                items: 1,
                errors: 0,
                ..Totals::default()
            }
        } else {
            Totals {
                apparent: 0,
                alloc: 0,
                items: 1,
                errors: 1,
                ..Totals::default()
            }
        };
        if let Some(&(bytes, out)) = homes.get(path) {
            t.alloc += bytes;
            t.shared_out += out;
        }
        if n.meta.kind == Kind::Dir {
            t.mounts = mount_root(*d) as u64;
            // Set when the directory was opened; check_subvol verified the flag.
            t.ro_subvols = by_path
                .get(path)
                .is_some_and(|&ix| tree.flags(ix).has(NodeFlags::RO_SUBVOL))
                as u64;
            if let Some(s) = state_of(path)
                && matches!(
                    s,
                    DirState::Failed | DirState::Partial | DirState::Abandoned
                )
            {
                t.errors += 1;
            }
            let mut prefix_len = path.len() + 1;
            if prefix_len == 0 {
                prefix_len = 1;
            }
            for (p, ct) in expect.iter() {
                if p.len() > prefix_len
                    && p.starts_with(path)
                    && p[path.len()] == SEP
                    && !p[prefix_len..].contains(&SEP)
                {
                    t.add(ct);
                }
            }
        }
        expect.insert(path.clone(), t);
    }

    let mut checked = 0;
    // Deepest first, so the reported mismatch is where the difference starts.
    let mut paths: Vec<&Vec<u8>> = expect.keys().collect();
    paths.sort_by_key(|p| (std::cmp::Reverse(p.len()), (*p).clone()));
    for path in paths {
        let t = &expect[path];
        let ix = *by_path
            .get(path)
            .ok_or_else(|| format!("missing entry {}", String::from_utf8_lossy(path)))?;
        let got = tree.total(ix);
        if got != *t {
            let in_tree: Vec<String> = tree
                .children(ix)
                .map(|c| String::from_utf8_lossy(tree.name(c)).into_owned())
                .collect();
            let mut expected: Vec<String> = expect
                .keys()
                .filter(|p| {
                    p.starts_with(path)
                        && p.len() > path.len()
                        && p[path.len()] == SEP
                        && !p[path.len() + 1..].contains(&SEP)
                })
                .map(|p| String::from_utf8_lossy(&p[path.len() + 1..]).into_owned())
                .collect();
            expected.sort();
            let state = tree.dir(ix).map(|d| d.state);
            if tree.dir(ix).is_none() {
                let key = tree.dev_ino(ix);
                let links: Vec<String> = by_path
                    .iter()
                    .filter(|&(_, &o)| tree.dir(o).is_none() && tree.dev_ino(o) == key)
                    .map(|(p, &o)| {
                        format!(
                            "{} charged={}",
                            String::from_utf8_lossy(p),
                            tree.flags(o).has(NodeFlags::CHARGED)
                        )
                    })
                    .collect();
                return Err(format!(
                    "{}: got {got:?} expected {t:?}; links of inode {key:?}: {links:?}",
                    String::from_utf8_lossy(path)
                ));
            }
            return Err(format!(
                "{}: got {got:?} expected {t:?}; state {state:?}; children {in_tree:?} expected {expected:?}",
                String::from_utf8_lossy(path)
            ));
        }
        checked += (tree.dir(ix).is_some()) as usize;
    }
    if by_path.len() != expect.len() {
        let extra: Vec<String> = by_path
            .keys()
            .filter(|p| !expect.contains_key(*p))
            .take(5)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        return Err(format!(
            "tree has {} entries, expected {}; extra e.g. {extra:?}",
            by_path.len(),
            expect.len()
        ));
    }
    Ok((checked, abandoned))
}
