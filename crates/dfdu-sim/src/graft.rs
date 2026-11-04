//! Scans of a folder above another scan's root that keep what it read
//! ([`dfdu_engine::reuse`]): a scan of a directory of the spec ([`scan_old`]), changes, and a
//! scan of the spec's root that starts from the old scan's tree grafted below it and resumes it
//! ([`scan_above`]), among random rescans, deletes and cancels. The old scan starts at a time
//! (`since`) when some directories' mtimes are not older: those may change within that second
//! without their mtimes moving, which the new scan must find all the same. The old root may be
//! removed, or a mount, which the new scan judges as a fresh scan does. The end is checked as
//! any run's end, against the spec.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use dfdu_engine::reuse::{self, Scanned};
use dfdu_engine::{Command, Model};
use dfdu_scan::{Backend as _, JobOptions};
use dfdu_tree::{HostPath, Ix, Kind, NodeRef, Tree};

use crate::resume::{below, pick, rel_path, restore_subtree};
use crate::{
    ROOT, ROOT_NAME, Rng, Sim, SimBackend, SimClock, SimConfig, SimError, SimReport, Spec,
};

/// The path of spec node `i`.
fn path_of(spec: &Spec, i: u32) -> HostPath {
    HostPath::local(&Path::new(ROOT_NAME).join(rel_path(spec, i)))
}

/// The directories below the root of `spec` whose paths name them for the backend, which
/// finds a path by its names as UTF-8 split at either separator.
fn named_dirs(spec: &Spec) -> Vec<u32> {
    let named = |n: &[u8]| std::str::from_utf8(n).is_ok() && !n.contains(&b'\\');
    let mut out = Vec::new();
    let mut stack = vec![ROOT];
    while let Some(i) = stack.pop() {
        for &c in &spec.nodes[i as usize].children {
            let n = &spec.nodes[c as usize];
            if n.meta.kind == Kind::Dir && named(&n.name) {
                out.push(c);
                stack.push(c);
            }
        }
    }
    out
}

/// The spec's root.
fn root_path() -> HostPath {
    HostPath::local(Path::new(ROOT_NAME))
}

/// Scans directory `old` of `spec` to the end with `cfg`'s mounts and no commands of its own,
/// as a scan started at `since` (seconds), and copies what it read for a scan of the spec's
/// root ([`reuse::scanned`]): `None` when it cannot be kept (the scan of it is not complete).
/// Returns it with the random stream to go on with and the mounts the scan entered
/// (`cross_mounts` and `network`, as drawn when `cfg` leaves them open).
///
/// # Errors
///
/// When a check of the scan fails.
pub fn scan_old(
    seed: u64,
    rng: Rng,
    cfg: &SimConfig,
    spec: Arc<Spec>,
    old: u32,
    since: u64,
) -> Result<(Option<Scanned>, Rng, bool, bool), SimError> {
    let quiet = SimConfig {
        rescans: 0,
        deletes: 0,
        cancels: 0,
        ..cfg.clone()
    };
    let mut sim = Sim::build(seed, rng, &quiet, spec.clone());
    let opts = JobOptions {
        class: None,
        ..sim.scan_opts()
    };
    sim.command(Command::Scan {
        path: path_of(&spec, old),
        opts,
    })?;
    sim.settle().map_err(|e| e.during("old scan"))?;
    let scanned = reuse::scanned(&sim.model, &root_path(), since);
    Ok((scanned, sim.rng, sim.cross_mounts, sim.network))
}

/// Scans the root of `spec` with `cfg`, keeping `scanned` where [`reuse::above`] lets it,
/// with the folders on the way stat'ed in `stat` (the same spec, or one as it was a moment
/// before), to the end, and checks it. A resume a cancel stopped is resumed again in full until
/// one is not. The report counts the old scan's directories still the loaded nodes at the end
/// (`kept`), and with `full_resumes` 1 a scan that kept nothing and started afresh.
///
/// # Errors
///
/// When a check fails.
pub fn scan_above(
    seed: u64,
    rng: Rng,
    cfg: &SimConfig,
    scanned: Option<Scanned>,
    stat: &Arc<Spec>,
    spec: Arc<Spec>,
) -> Result<SimReport, SimError> {
    let mut sim = Sim::build(seed, rng, cfg, spec);
    let opts = sim.scan_opts();
    let stats = SimBackend {
        spec: stat.clone(),
        clock: Arc::new(SimClock(AtomicU64::new(1))),
        wrong_storage: AtomicU64::new(0),
    };
    let start = scanned.and_then(|s| {
        let old = s.root();
        let (tree, changed, since) = reuse::above(s, &root_path(), |p| {
            stats.stat_root(&stats.loc_from_path(&p.to_local())).ok()
        })?;
        Some((tree, changed, since, old))
    });
    let Some((tree, changed, since, old)) = start else {
        sim.report.full_resumes = 1;
        sim.command(Command::Scan {
            path: root_path(),
            opts,
        })?;
        return sim.run_to_end();
    };
    let dirs = dirs_below(&tree, &old);
    let Some(root) = tree.roots().next() else {
        return Err(SimError::new(seed, "the graft has no root"));
    };
    let node = tree.node_ref(root);
    sim.model.tree = tree;
    sim.command(Command::Resume {
        node,
        opts: opts.clone(),
        changed: Some(changed),
        since,
        root: None,
    })?;
    sim.settle()?;
    let mut cancels = 0;
    while sim.report.cancels > cancels {
        cancels = sim.report.cancels;
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
    sim.report.kept = kept(&sim.model, &dirs);
    sim.finish()
}

/// How many of `dirs` are still visible nodes.
fn kept(m: &Model, dirs: &[NodeRef]) -> u64 {
    dirs.iter()
        .filter(|&&r| m.tree.resolve_visible(r).is_some())
        .count() as u64
}

/// Every directory of `tree` at or below the one at `path`.
fn dirs_below(tree: &Tree, path: &HostPath) -> Vec<NodeRef> {
    let Some(root) = tree.roots().next() else {
        return Vec::new();
    };
    let mut at = root;
    for name in path.rel(&tree.host_path(root)).unwrap_or_default() {
        match tree.children(at).find(|&c| tree.name(c) == name) {
            Some(c) => at = c,
            None => return Vec::new(),
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<Ix> = vec![at];
    while let Some(ix) = stack.pop() {
        out.push(tree.node_ref(ix));
        stack.extend(tree.children(ix).filter(|&c| tree.dir(c).is_some()));
    }
    out
}

/// Runs one seed: a scan of a directory of a generated spec, changes, and a scan of the
/// spec's root keeping it, in random order. Sometimes the old root is removed first.
///
/// # Errors
///
/// When a check of either scan fails; the message names the seed and what went wrong.
pub fn run_grafted(seed: u64, cfg: &SimConfig) -> Result<SimReport, SimError> {
    let mut rng = Rng::new(seed);
    let mut spec = Spec::generate(&mut rng, &cfg.spec, cfg.limits.block_entries);
    // Directories last changed over a few seconds, around when the old scan starts.
    for n in &mut spec.nodes {
        if n.meta.kind == Kind::Dir {
            n.meta.mtime = 1 + i64::try_from(rng.below(8).saturating_sub(5)).unwrap_or(0);
        }
    }
    let spec = Arc::new(spec);
    let dirs = named_dirs(&spec);
    if dirs.is_empty() {
        return Sim::start(seed, rng, cfg, spec).run_to_end();
    }
    let old = dirs[rng.index(dirs.len())];
    // What is created after the old scan does not exist yet.
    let added = pick(&spec, &mut rng, &[old]);
    for &i in &added {
        spec.delete_subtree(i);
    }
    let since = 2 + rng.below(3);
    let (scanned, mut rng, cross_mounts, network) =
        scan_old(seed, rng, cfg, spec.clone(), old, since)?;

    let mut changed = Vec::new();
    for &i in &added {
        restore_subtree(&spec, i);
        changed.push(spec.parent[i as usize]);
    }
    for i in pick(&spec, &mut rng, &added) {
        spec.delete_subtree(i);
        changed.push(spec.parent[i as usize]);
    }
    // The old root removed, unless something above was created or removed with it.
    if rng.chance(100)
        && spec.deletable(old)
        && !added
            .iter()
            .any(|&a| below(&spec, old, a) || below(&spec, a, old))
    {
        spec.delete_subtree(old);
        changed.push(spec.parent[old as usize]);
    }
    // Each change marks its directory's mtime, unless made within the second the old scan
    // started, in a directory whose mtime is that second or later.
    for &d in &changed {
        let mtime = u64::try_from(spec.nodes[d as usize].meta.mtime).unwrap_or(0);
        if mtime < since || !rng.chance(500) {
            spec.touch(d);
        }
    }
    let cfg = SimConfig {
        cross_mounts: Some(cross_mounts),
        network: Some(network),
        ..cfg.clone()
    };
    scan_above(seed, rng, &cfg, scanned, &spec.clone(), spec)
}
