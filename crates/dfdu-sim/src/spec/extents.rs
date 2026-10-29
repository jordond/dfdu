//! Shared data in generated specs: extent maps, reflinked copies and snapshots.

use std::collections::HashMap;

use dfdu_tree::Kind;

use super::{Fault, ROOT, Rng, SimPiece, SpecNode, id32};

/// Extent maps: fresh extents per inode (hard links share them), then reflinked copies of part
/// of another file's data, then snapshots: some subvolumes get a copy of another directory's
/// contents with the same inodes and extents.
pub(super) fn gen_extents(
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
pub(super) fn mark_shared(rng: &mut Rng, nodes: &mut [SpecNode]) {
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

pub(super) fn copy_subtree(nodes: &mut Vec<SpecNode>, n: u32) -> u32 {
    let mut node = nodes[n as usize].clone();
    node.children = node
        .children
        .iter()
        .map(|&c| copy_subtree(nodes, c))
        .collect();
    nodes.push(node);
    id32(nodes.len() - 1)
}

#[cfg(test)]
mod tests;
