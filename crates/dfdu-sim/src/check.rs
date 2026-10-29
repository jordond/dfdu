//! The checks made after the run: classes, pool flags, subvolumes and every directory's totals
//! against a reference computed from the spec.

use std::collections::{HashMap, VecDeque};

use dfdu_engine::Model;
use dfdu_sys::FsClass;
use dfdu_tree::{DirState, Ix, Kind, NodeFlags, SEP, Totals, Tree};

use crate::{Fault, ROOT, ROOT_NAME, Rng, Spec, SpecNode};

mod classes;

pub use classes::sim_check;
pub(crate) use classes::{check_classes, check_pool_flags};

/// The visible directory closest to the root that a cancelled job left incomplete.
pub(crate) fn topmost_incomplete(tree: &Tree) -> Option<Ix> {
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

pub(crate) fn random_dir(tree: &Tree, rng: &mut Rng) -> Option<Ix> {
    let root = tree.roots().next()?;
    let mut dirs = vec![];
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        dirs.push(d);
        stack.extend(tree.children(d).filter(|&c| tree.dir(c).is_some()));
    }
    dirs.get(rng.index(dirs.len())).copied()
}

/// Shared-extent accounting by brute force: which files are measured
/// (with their unshared bytes), and the shared bytes counted at each path, with the part shared
/// only outside the scan.
#[expect(
    clippy::type_complexity,
    reason = "maps keyed by path and extent, each used once in this function"
)]
#[expect(
    clippy::expect_used,
    reason = "every path is below the root's, so the common prefix shrinks to a separator"
)]
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
        bounds.sort_unstable();
        bounds.dedup();
        for w in bounds.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut refs: Vec<&Vec<u8>> = rs
                .iter()
                .filter(|r| r.0 <= a && b <= r.1)
                .map(|r| r.2)
                .collect();
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
    use DirState::{Abandoned, Complete, Failed, Partial};
    // A stat-phase fault in a block past the end is never reached.
    let exists = |b: usize| b * block < len;
    match fault {
        // An error after the last entry is never reached.
        Fault::EioAfter(k) if *k >= len => state == Complete,
        Fault::Eacces | Fault::EioAfter(0) => state == Failed,
        Fault::EioAfter(_) | Fault::PanicAtBlock(_) | Fault::PanicOnOpen => state == Partial,
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
        Fault::None | Fault::StatPanic(_) | Fault::StatStall { .. } => state == Complete,
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

/// `p` is a direct child of `dir`.
fn is_child(p: &[u8], dir: &[u8]) -> bool {
    p.len() > dir.len() + 1
        && p.starts_with(dir)
        && p[dir.len()] == SEP
        && !p[dir.len() + 1..].contains(&SEP)
}

/// Whether spec directory `d` is the root of a mount: by mount id where the spec has them,
/// else by device.
fn mount_root(spec: &Spec, d: u32) -> bool {
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
}

/// Compares every directory in the final tree with the spec. Returns (dirs checked, abandoned).
pub(crate) fn compare(
    spec: &Spec,
    model: &Model,
    block: usize,
    cross: bool,
    network: bool,
) -> Result<(usize, u64), String> {
    let tree = &model.tree;
    let root = tree.roots().next().ok_or("no root")?;
    let mut by_path: HashMap<Vec<u8>, Ix> = HashMap::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        by_path.insert(tree.path(ix), ix);
        stack.extend(tree.children(ix));
    }
    let mut seen = walk_and_check_flags(spec, model, &by_path, block, cross, network)?;
    let expect = expected_totals(spec, tree, &by_path, &mut seen);
    let checked = diff_totals(tree, &by_path, &expect)?;
    Ok((checked, seen.abandoned))
}

/// What the walk over the spec found.
struct Seen {
    /// The spec entries the scan saw, with their paths.
    entries: Vec<(u32, Vec<u8>)>,
    /// The lowest path of every hard-linked inode.
    charged: HashMap<(u64, u64), Vec<u8>>,
    abandoned: u64,
}

/// Walks the spec along the directories the scan saw, checking each one's mount, class and
/// subvolume flags and its final state against its fault.
fn walk_and_check_flags(
    spec: &Spec,
    model: &Model,
    by_path: &HashMap<Vec<u8>, Ix>,
    block: usize,
    cross: bool,
    network: bool,
) -> Result<Seen, String> {
    let tree = &model.tree;
    let state_of = |p: &[u8]| by_path.get(p).and_then(|&ix| tree.dir(ix)).map(|d| d.state);
    // With cross_mounts, how many times each mounted directory was descended into.
    let mut descended: HashMap<(u64, u64), u32> = HashMap::new();
    let mut seen = Seen {
        entries: vec![],
        charged: HashMap::new(),
        abandoned: 0,
    };
    let mut stack = vec![(ROOT, ROOT_NAME.as_bytes().to_vec())];
    while let Some((d, path)) = stack.pop() {
        let n = &spec.nodes[d as usize];
        seen.entries.push((d, path.clone()));
        if n.meta.kind != Kind::Dir {
            if !n.vanished && n.meta.nlink > 1 {
                let e = seen
                    .charged
                    .entry((n.meta.dev, n.meta.ino))
                    .or_insert_with(|| path.clone());
                if path < *e {
                    e.clone_from(&path);
                }
            }
            continue;
        }
        let ix = *by_path
            .get(&path)
            .ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&path)))?;
        check_subvol(spec, model, d, ix, &path)?;
        let flags = tree.flags(ix);
        if !check_mount(spec, d, &path, flags, state_of(&path), cross, network)? {
            continue;
        }
        if mount_root(spec, d) {
            *descended.entry((n.meta.dev, n.meta.ino)).or_default() += 1;
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
        seen.abandoned += u64::from(state == DirState::Abandoned);
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
    Ok(seen)
}

/// Checks the mount root, network, kernel and duplicate-mount flags of spec directory `d`
/// (`flags` in the tree, final state `state`). Returns whether the scan descends into it; a
/// mount it must skip is checked to be skipped.
fn check_mount(
    spec: &Spec,
    d: u32,
    path: &[u8],
    flags: NodeFlags,
    state: Option<DirState>,
    cross: bool,
    network: bool,
) -> Result<bool, String> {
    let n = &spec.nodes[d as usize];
    let shown = || String::from_utf8_lossy(path);
    let mount = mount_root(spec, d);
    if flags.has(NodeFlags::MOUNT_ROOT) != mount {
        return Err(format!(
            "{}: mount root flag {} for a {}",
            shown(),
            flags.has(NodeFlags::MOUNT_ROOT),
            if mount { "mount" } else { "directory" }
        ));
    }
    // With mount ids the duplicates are known; without, the first mount seen wins.
    let dup = if n.meta.mount != 0 {
        n.dup_mount
    } else {
        flags.has(NodeFlags::DUP_MOUNT)
    };
    // Kernel filesystems are never descended into, network ones only with `network`; the
    // class is known only for mount roots the backend can name.
    let class = if mount { n.fs_class } else { None };
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
                shown(),
                flags.has(flag)
            ));
        }
    }
    if !mount {
        if flags.has(NodeFlags::DUP_MOUNT) {
            return Err(format!("{}: marked as a duplicate mount", shown()));
        }
        return Ok(true);
    }
    if wanted && flags.has(NodeFlags::DUP_MOUNT) != dup {
        return Err(format!(
            "{}: duplicate-mount flag {} expected {dup}",
            shown(),
            flags.has(NodeFlags::DUP_MOUNT)
        ));
    }
    if !wanted || dup {
        if state != Some(DirState::Skipped) {
            return Err(format!("{}: mount not skipped", shown()));
        }
        return Ok(false);
    }
    Ok(true)
}

/// Every seen entry's expected totals, bottom-up. Sorts `seen.entries` deepest first.
fn expected_totals(
    spec: &Spec,
    tree: &Tree,
    by_path: &HashMap<Vec<u8>, Ix>,
    seen: &mut Seen,
) -> HashMap<Vec<u8>, Totals> {
    let (measured, homes) = expected_homes(spec, &seen.entries, &seen.charged);
    let mut expect: HashMap<Vec<u8>, Totals> = HashMap::new();
    seen.entries.sort_by_key(|s| std::cmp::Reverse(s.1.len()));
    for (d, path) in &seen.entries {
        let n = &spec.nodes[*d as usize];
        let mut t = if n.meta.kind == Kind::Dir || !n.vanished {
            let counted = n.meta.kind == Kind::Dir
                || n.meta.nlink <= 1
                || seen.charged.get(&(n.meta.dev, n.meta.ino)) == Some(path);
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
            let ix = by_path.get(path).copied();
            t.mounts = u64::from(mount_root(spec, *d));
            // Set when the directory was opened; check_subvol verified the flag.
            t.ro_subvols = u64::from(ix.is_some_and(|ix| tree.flags(ix).has(NodeFlags::RO_SUBVOL)));
            if let Some(s) = ix.and_then(|ix| tree.dir(ix)).map(|d| d.state)
                && matches!(
                    s,
                    DirState::Failed | DirState::Partial | DirState::Abandoned
                )
            {
                t.errors += 1;
            }
            for (p, ct) in &expect {
                if is_child(p, path) {
                    t.add(ct);
                }
            }
        }
        expect.insert(path.clone(), t);
    }
    expect
}

/// Compares the tree's totals with `expect`, deepest first so the reported mismatch is where
/// the difference starts, and checks the tree has no other entries. Returns the number of
/// directories checked.
fn diff_totals(
    tree: &Tree,
    by_path: &HashMap<Vec<u8>, Ix>,
    expect: &HashMap<Vec<u8>, Totals>,
) -> Result<usize, String> {
    let mut checked = 0;
    let mut paths: Vec<&Vec<u8>> = expect.keys().collect();
    paths.sort_by_key(|p| (std::cmp::Reverse(p.len()), (*p).clone()));
    for path in paths {
        let t = &expect[path];
        let ix = *by_path
            .get(path)
            .ok_or_else(|| format!("missing entry {}", String::from_utf8_lossy(path)))?;
        let got = tree.total(ix);
        if got != *t {
            return Err(mismatch(tree, by_path, expect, path, ix, &got, t));
        }
        checked += usize::from(tree.dir(ix).is_some());
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
    Ok(checked)
}

/// Describes a totals mismatch at `path`: for a file, every link of its inode and which one
/// is charged; for a directory, its state and its children in the tree and in the spec.
fn mismatch(
    tree: &Tree,
    by_path: &HashMap<Vec<u8>, Ix>,
    expect: &HashMap<Vec<u8>, Totals>,
    path: &[u8],
    ix: Ix,
    got: &Totals,
    t: &Totals,
) -> String {
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
        return format!(
            "{}: got {got:?} expected {t:?}; links of inode {key:?}: {links:?}",
            String::from_utf8_lossy(path)
        );
    }
    let in_tree: Vec<String> = tree
        .children(ix)
        .map(|c| String::from_utf8_lossy(tree.name(c)).into_owned())
        .collect();
    let mut expected: Vec<String> = expect
        .keys()
        .filter(|p| is_child(p, path))
        .map(|p| String::from_utf8_lossy(&p[path.len() + 1..]).into_owned())
        .collect();
    expected.sort();
    let state = tree.dir(ix).map(|d| d.state);
    format!(
        "{}: got {got:?} expected {t:?}; state {state:?}; children {in_tree:?} expected {expected:?}",
        String::from_utf8_lossy(path)
    )
}
