//! Resumed runs: a scan of a spec, its tree loaded as a snapshot would load it, the spec
//! changed (entries created and removed, their directories' mtimes moved on), and a resume of
//! the loaded tree with what a change journal would say, among random rescans, deletes and
//! cancels. The journal sometimes cannot tell (everything is scanned again), names directories
//! that did not change or are not there, and loses changed directories anywhere, or all of
//! them, which the resume must find by their mtimes. The journal position is read at a time
//! (`since`) when it can tell: a directory whose mtime is not older may change within that
//! same second without its mtime moving, which the resume must find all the same. The end is
//! checked as any run's end, against the spec: what a fresh scan of it gives.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use dfdu_engine::Command;
use dfdu_scan::Backend as _;
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
    let mut spec = Spec::generate(&mut rng, &cfg.spec, cfg.limits.block_entries);
    // Directories last changed over a few seconds, around when the journal position is read.
    for n in &mut spec.nodes {
        if n.meta.kind == Kind::Dir {
            n.meta.mtime = 1 + i64::from(rng.below(3) == 0) + i64::from(rng.below(3) == 0);
        }
    }
    let spec = Arc::new(spec);
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
    // When the journal position was read, before the scan; 0 when not known.
    let since = if rng.chance(250) { 0 } else { 1 + rng.below(3) };

    // The changes since: each marks its directory's mtime, unless made within the second the
    // position was read, in a directory whose mtime is that second or later.
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
        let mtime = u64::try_from(spec.nodes[d as usize].meta.mtime).unwrap_or(0);
        if since == 0 || mtime < since || !rng.chance(500) {
            spec.touch(d);
        }
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
    // Stat'ed after the journal was asked, as the engine's journal thread does.
    let stat = sim.shared.backend.stat_root(&ROOT).ok();
    sim.command(Command::Resume {
        node,
        opts: opts.clone(),
        changed: answer,
        since,
        root: stat,
    })?;
    sim.settle()?;
    // A cancelled resume leaves what it had not listed again as loaded, out of date where it
    // changed, refused to deletes and the scan incomplete: it is resumed again, in full, until
    // one is not cancelled (the cancels run out).
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
            since: 0,
            root: None,
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
/// it cannot tell. Otherwise it names them, loses some anywhere (with 1 in 10 all of them, as
/// `FSEvents` can on a loaded machine), and names a few more that did not change or do not
/// exist.
fn journal(spec: &Spec, rng: &mut Rng, mut changed: Vec<u32>) -> (Option<Vec<PathBuf>>, u64) {
    if rng.chance(100) {
        return (None, 0);
    }
    changed.sort_unstable();
    changed.dedup();
    let all = rng.chance(100);
    let (mut out, mut lost) = (Vec::new(), 0);
    for d in changed {
        if all || rng.chance(250) {
            lost += 1;
        } else {
            out.push(rel_path(spec, d));
        }
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
