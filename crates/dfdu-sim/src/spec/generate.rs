//! Seeded random specs: the tree with its mounts, subvolumes, hard links and faults.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;

use dfdu_sys::{FsClass, Storage};
use dfdu_tree::{Kind, Meta, Subvol};

use super::extents::{copy_subtree, gen_extents, mark_shared};
use super::{CACHEDIR_TAG, Fault, ROOT, ROOT_NAME, Rng, Spec, SpecNode, id32};

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
        let root = Meta {
            kind: Kind::Dir,
            apparent: 4096,
            alloc: 4096,
            mtime: 1,
            dev: 1,
            ino,
            nlink: 2,
            mount: u64::from(cfg.mount_ids),
        };
        let mut nodes = vec![SpecNode {
            storage: [
                Storage::Solid,
                Storage::Rotational,
                Storage::Network,
                Storage::Unknown,
            ][rng.index(4)],
            ..SpecNode::new(ROOT_NAME.as_bytes().to_vec(), root)
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
                let meta = Meta {
                    kind: Kind::Dir,
                    apparent: 4096,
                    alloc: 4096,
                    mtime: 1,
                    dev: cdev,
                    ino: cino,
                    nlink: 2,
                    mount: cmount,
                };
                nodes.push(SpecNode {
                    subvol,
                    fs_class,
                    storage,
                    ..SpecNode::new(pick_name(rng, cfg, DIR_NAMES, "d", i), meta)
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
                let meta = Meta {
                    kind: Kind::File,
                    apparent: size,
                    alloc,
                    mtime: 1,
                    dev,
                    ino,
                    nlink: 1,
                    mount: 0,
                };
                // Fields are evaluated in the order written: `vanished` draws first.
                nodes.push(SpecNode {
                    vanished: cfg.faults && rng.chance(30),
                    unmeasurable: cfg.faults && rng.chance(30),
                    content,
                    ..SpecNode::new(name, meta)
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
        let touched = (0..nodes.len()).map(|_| AtomicBool::new(false)).collect();
        Spec {
            nodes,
            dev_fs,
            parent,
            deleted,
            touched,
        }
    }
}

#[cfg(test)]
mod tests;
