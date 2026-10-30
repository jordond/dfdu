//! Resumed runs: a scan of a spec, its tree loaded as a snapshot would load it, the spec
//! changed (entries created and removed, their directories' mtimes moved on), and a resume of
//! the loaded tree with what a change journal would say, among random rescans, deletes and
//! cancels. The journal sometimes cannot tell (everything is scanned again), names directories
//! that did not change or are not there, and loses changed directories whose parent is listed
//! again, which the resume must find by their mtimes. The end is checked as any run's end,
//! against the spec: what a fresh scan of it gives.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use dfdu_engine::Command;
use dfdu_tree::{DirState, Ix, Kind, Meta, NodeFlags, NodeRef, Tree};

use crate::{ROOT, Rng, Sim, SimConfig, SimReport, Spec};

/// At most this many entries are created after the snapshot, and as many removed.
const CHANGES: u64 = 5;

/// Runs one seed: a scan, a snapshot of it, changes, and a resume, in random order.
///
/// # Errors
///
/// When a check of either run fails; the message names the seed and what went wrong.
pub fn run_resumed(seed: u64, cfg: &SimConfig) -> Result<SimReport, String> {
    let mut rng = Rng::new(seed);
    let spec = Arc::new(Spec::generate(
        &mut rng,
        &cfg.spec,
        cfg.limits.block_entries,
    ));
    // What is created after the snapshot does not exist yet.
    let added = pick(&spec, &mut rng, &[]);
    for &i in &added {
        spec.delete_subtree(i);
    }
    // The scan the snapshot is taken of, with no commands of its own.
    let quiet = SimConfig {
        rescans: 0,
        deletes: 0,
        cancels: 0,
        ..cfg.clone()
    };
    let mut first = Sim::start(seed, rng, &quiet, spec.clone());
    first.settle().map_err(|e| format!("scan: {e}"))?;
    let tree = loaded_copy(&first.model.tree);
    let resumed = SimConfig {
        cross_mounts: Some(first.cross_mounts),
        network: Some(first.network),
        ..cfg.clone()
    };
    let mut rng = first.rng;

    // The changes since: each marks its directory's mtime.
    let mut changed = Vec::new();
    for &i in &added {
        restore_subtree(&spec, i);
        changed.push(spec.parent[i as usize]);
    }
    for i in pick(&spec, &mut rng, &added) {
        spec.delete_subtree(i);
        changed.push(spec.parent[i as usize]);
    }
    for &d in &changed {
        spec.touch(d);
    }
    let (answer, lost) = journal(&spec, &mut rng, changed);

    let mut sim = Sim::build(seed, rng, &resumed, spec);
    let Some(root) = tree.roots().next() else {
        return Err(format!("seed {seed}: the scan left no root"));
    };
    let node = tree.node_ref(root);
    let dirs = all_dirs(&tree);
    sim.model.tree = tree;
    sim.report.lost_events = lost;
    sim.report.full_resumes = u64::from(answer.is_none());
    let opts = sim.scan_opts();
    sim.command(Command::Resume {
        node,
        opts: opts.clone(),
        changed: answer,
    })?;
    sim.settle()?;
    // A cancelled resume leaves what it had not listed again as loaded, out of date where it
    // changed: it is resumed again, in full, until one is not cancelled (the cancels run out).
    let mut cancels = 0;
    while sim.report.cancels > cancels {
        cancels = sim.report.cancels;
        // A rescan of the root replaced the loaded one.
        let t = &sim.model.tree;
        let node = t.roots().next().map_or(node, |r| t.node_ref(r));
        sim.command(Command::Resume {
            node,
            opts: opts.clone(),
            changed: None,
        })?;
        sim.settle()?;
    }
    let t = &sim.model.tree;
    sim.report.kept = dirs
        .iter()
        .filter(|&&r| t.resolve_visible(r).is_some())
        .count() as u64;
    sim.finish()
}

/// Up to [`CHANGES`] entries to create or remove: live, in a directory without faults (so
/// that no fault moves), and not inside or above one another or one of `not`.
fn pick(spec: &Spec, rng: &mut Rng, not: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let n = spec.nodes.len();
    for _ in 0..rng.below(CHANGES + 1) {
        let i = id(rng.index(n));
        let related = |j: u32| below(spec, i, j) || below(spec, j, i);
        if spec.deletable(i) && !out.iter().chain(not).any(|&j| related(j)) {
            out.push(i);
        }
    }
    out
}

/// A spec node's number as the spec stores it.
fn id(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(ROOT)
}

/// Whether `i` is `a` or below it.
fn below(spec: &Spec, mut i: u32, a: u32) -> bool {
    loop {
        if i == a {
            return true;
        }
        if i == ROOT {
            return false;
        }
        i = spec.parent[i as usize];
    }
}

fn restore_subtree(spec: &Spec, i: u32) {
    let mut stack = vec![i];
    while let Some(n) = stack.pop() {
        spec.restore(n);
        stack.extend(&spec.nodes[n as usize].children);
    }
}

/// The path of `i` relative to the root.
fn rel_path(spec: &Spec, mut i: u32) -> PathBuf {
    let mut names = Vec::new();
    while i != ROOT {
        names.push(String::from_utf8_lossy(&spec.nodes[i as usize].name).into_owned());
        i = spec.parent[i as usize];
    }
    names.iter().rev().collect()
}

/// What the journal says about the `changed` directories, and how many it lost. With 1 in 10
/// it cannot tell. Otherwise it names them, loses some whose parent is listed again (the
/// root's change is found by its mtime too), and names a few more that did not change or do
/// not exist.
fn journal(spec: &Spec, rng: &mut Rng, mut changed: Vec<u32>) -> (Option<Vec<PathBuf>>, u64) {
    if rng.chance(100) {
        return (None, 0);
    }
    let depth = |mut i: u32| {
        let mut d = 0;
        while i != ROOT {
            i = spec.parent[i as usize];
            d += 1;
        }
        d
    };
    changed.sort_unstable_by_key(|&d| (depth(d), d));
    changed.dedup();
    let (mut out, mut lost) = (Vec::new(), 0);
    let mut listed: HashSet<u32> = HashSet::new();
    for d in changed {
        let found = d == ROOT || listed.contains(&spec.parent[d as usize]);
        if found && rng.chance(250) {
            lost += 1;
        } else {
            out.push(rel_path(spec, d));
        }
        listed.insert(d);
    }
    for _ in 0..rng.below(3) {
        let i = id(rng.index(spec.nodes.len()));
        if spec.nodes[i as usize].meta.kind == Kind::Dir {
            out.push(rel_path(spec, i));
        }
    }
    if rng.chance(200) {
        out.push(PathBuf::from("not/there"));
    }
    (Some(out), lost)
}

/// Every directory of `tree`'s first root.
fn all_dirs(tree: &Tree) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let mut stack: Vec<Ix> = tree.roots().take(1).collect();
    while let Some(ix) = stack.pop() {
        out.push(tree.node_ref(ix));
        stack.extend(tree.children(ix).filter(|&c| tree.dir(c).is_some()));
    }
    out
}

/// The visible tree of `from` as a snapshot of it would load: every entry with its
/// metadata and error, each directory with the flags a snapshot keeps and its state (one still
/// being read as partial); no classes, no jobs, no measured extents, and a hard link charged
/// where it is first met.
fn loaded_copy(from: &Tree) -> Tree {
    const KEPT: [u16; 4] = [
        NodeFlags::MOUNT_ROOT,
        NodeFlags::KERNEL,
        NodeFlags::NETWORK,
        NodeFlags::DUP_MOUNT,
    ];
    let meta = |ix: Ix| {
        let (apparent, alloc) = from.raw_size(ix);
        let (dev, ino) = from.dev_ino(ix);
        Meta {
            kind: from.kind(ix),
            apparent,
            alloc,
            mtime: from.mtime(ix),
            dev,
            ino,
            nlink: if from.flags(ix).has(NodeFlags::HARDLINK) {
                2
            } else {
                1
            },
            mount: 0,
        }
    };
    let mut t = Tree::new();
    let Some(root) = from.roots().next() else {
        return t;
    };
    let r = t.new_root(from.name(root), &meta(root), true);
    if let Some(e) = from.error(root) {
        t.set_error(r, e);
    }
    let mut links = HashSet::new();
    let mut stack = vec![(root, r)];
    let mut dirs = vec![(root, r)];
    while let Some((f, to)) = stack.pop() {
        for c in from.children(f) {
            let m = meta(c);
            if from.dir(c).is_some() {
                let flags = KEPT
                    .iter()
                    .filter(|&&k| from.flags(c).has(k))
                    .fold(0, |a, &k| a | k);
                let ix = t.add_dir_flagged(to, from.name(c), &m, flags);
                let own = t.own(ix);
                t.propagate_add(to, &own);
                // As a snapshot sets it: after the totals, which it updates.
                if let Some(e) = from.error(c) {
                    t.set_error(ix, e);
                }
                stack.push((c, ix));
                dirs.push((c, ix));
            } else {
                let charged = !m.is_hardlink() || links.insert((m.dev, m.ino));
                let ix = t.add_file(to, from.name(c), &m, charged, from.error(c));
                let own = t.own(ix);
                t.propagate_add(to, &own);
            }
        }
    }
    for (f, ix) in dirs {
        let state = match from.dir(f).map(|d| d.state) {
            Some(DirState::Pending | DirState::Deleting | DirState::Superseded) | None => {
                DirState::Partial
            }
            Some(s) => s,
        };
        if let Some(d) = t.dir_mut(ix) {
            d.state = state;
            d.outcome = state;
            d.pending = 0;
            d.listing_closed = true;
            d.reported = true;
        }
    }
    t
}
