//! The checks made after the run: classes, pool flags, subvolumes and every directory's totals
//! against a reference computed from the spec.

use std::collections::{HashMap, HashSet, VecDeque};

use dfdu_engine::{CheckMsg, CheckReq, Model};
use dfdu_rules::{Candidate, Inside, RuleSet, Verdict};
use dfdu_scan::FsClass;
use dfdu_tree::{Class, DirState, Ix, Kind, NodeFlags, SEP, Totals, Tree};

use crate::{Fault, ROOT, ROOT_NAME, Rng, Spec, SpecNode};

/// Every node's class in effect and protection against a reference classifier run on the spec,
/// over every directory whose listing and whose parents' listings completed (a directory left
/// partial was classified on what was read). Comparing the class in effect, the match highest
/// above a node, leaves out matches below another match, which the scan makes only when the
/// match above was decided after its listing's subdirectories were queued (`dfdu_scan::classify`).
/// Returns the number of classified nodes.
#[expect(
    clippy::too_many_lines,
    reason = "one walk of the tree carrying each node's rule state down"
)]
pub(crate) fn check_classes(spec: &Spec, model: &Model, rules: &RuleSet) -> Result<u64, String> {
    use dfdu_rules::protected::State;
    let tree = &model.tree;
    let Some(root) = tree.roots().next() else {
        return Ok(0);
    };
    let complete = |ix: Ix| tree.dir(ix).is_some_and(|d| d.state == DirState::Complete);
    // (spec node, tree node, its path, rules decided above, protection, class in effect)
    let mut stack = vec![(
        ROOT,
        root,
        ROOT_NAME.as_bytes().to_vec(),
        Vec::<dfdu_rules::RuleIx>::new(),
        State::Off,
        None::<Class>,
    )];
    let mut classified = 0;
    while let Some((sn, ix, path, inside, prot, above)) = stack.pop() {
        if !complete(ix) {
            continue;
        }
        let kids: Vec<u32> = spec.live_children(sn).collect();
        let names: HashSet<&[u8]> = kids
            .iter()
            .map(|&c| &spec.nodes[c as usize].name[..])
            .collect();
        let sibling = |n: &[u8]| names.contains(n);
        let dir = rules.dir(&path);
        for &c in &kids {
            let cn = &spec.nodes[c as usize];
            let is_dir = cn.meta.kind == Kind::Dir;
            let cix = tree
                .children(ix)
                .find(|&t| tree.name(t) == &cn.name[..])
                .ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&cn.name)))?;
            let cpath = [&path[..], b"/", &cn.name].concat();
            let show = || String::from_utf8_lossy(&cpath).into_owned();
            let verdict = dir.classify(&Candidate {
                parent: &path,
                name: &cn.name,
                is_dir,
                sibling: &sibling,
                inside: &inside,
                protection: prot,
                partial: false,
                stat: (!is_dir).then_some(dfdu_rules::Stat {
                    disk: cn.meta.alloc,
                    mtime: cn.meta.mtime,
                }),
                link: None,
            });
            let (mut cprot, mut own) = (State::Off, None);
            match verdict {
                Verdict::Protected(st) => cprot = st,
                Verdict::None => {}
                Verdict::Hits(hits) => {
                    // Its own listing as the scan saw it: vanished entries are listed but
                    // not stat'ed, so they are there but are not files to read.
                    let inner: Vec<u32> = if is_dir {
                        spec.live_children(c).collect()
                    } else {
                        vec![]
                    };
                    let child = |n: &[u8]| inner.iter().any(|&k| spec.nodes[k as usize].name == n);
                    let marker = |f: &[u8], len: usize| {
                        let k = inner.iter().find(|&&k| spec.nodes[k as usize].name == f)?;
                        let n = &spec.nodes[*k as usize];
                        (n.meta.kind == Kind::File && !n.vanished)
                            .then(|| n.content[..len.min(n.content.len())].to_vec())
                    };
                    // Listings below that are not complete were looked into on what was read.
                    let looked = !is_dir || complete(cix);
                    own = hits.iter().find_map(|h| {
                        let ok = match h.inside {
                            Inside::Done => true,
                            Inside::Check => {
                                is_dir
                                    && rules.check_inside(
                                        h.rule,
                                        &cn.name,
                                        &child,
                                        &marker,
                                        inner.is_empty(),
                                    )
                            }
                        };
                        ok.then_some(Class {
                            rule: h.rule,
                            safety: h.safety,
                            degraded: h.degraded,
                            flags: 0,
                        })
                    });
                    if !looked {
                        continue;
                    }
                }
            }
            let mut want = above.or(own);
            // The classifier pool's flags, and the protection a git-tracked match gets, are not
            // the scan's to decide (`check_pool_flags`).
            let got = tree
                .class_in_effect(cix)
                .map(|(_, c)| Class { flags: 0, ..c });
            if let (Some(w), Some((_, c))) = (want.as_mut(), tree.class_in_effect(cix))
                && c.has(Class::VCS_TRACKED)
            {
                w.safety = c.safety;
            }
            if got != want {
                return Err(format!("{}: class {got:?}, expected {want:?}", show()));
            }
            if tree.is_protected(cix) != cprot.protected() {
                return Err(format!("{}: protected {}", show(), !cprot.protected()));
            }
            classified += u64::from(own.is_some());
            if is_dir {
                let mut ci = inside.clone();
                ci.extend(own.map(|c| c.rule));
                stack.push((c, cix, cpath, ci, cprot, want));
            }
        }
    }
    Ok(classified)
}

/// The classifier pool's answer in the simulation, decided by a hash of the node's path: git
/// tracks a third of guarded matches, cannot tell for another third, and a process a rule names
/// runs for a quarter. An answer applied to another node than it was asked for shows up as
/// flags that do not match that node's path.
#[must_use]
pub fn sim_check(req: &CheckReq) -> CheckMsg {
    match req {
        CheckReq::Guards {
            node,
            rule,
            path,
            vcs,
            processes,
        } => {
            let (tracked, running) =
                pool_answer(path.as_os_str().as_encoded_bytes(), *vcs, processes);
            CheckMsg::Guards {
                node: *node,
                rule: *rule,
                tracked,
                running,
            }
        }
        CheckReq::NativeSize { rule, .. } => CheckMsg::NativeSize {
            rule: *rule,
            result: Ok(0),
        },
    }
}

fn pool_answer(path: &[u8], vcs: bool, processes: &[String]) -> (Option<bool>, Option<String>) {
    let h = path.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    let tracked = match (vcs, h % 3) {
        (false, _) | (true, 1) => Some(false),
        (true, 0) => Some(true),
        _ => None,
    };
    let running = (h / 3 % 4 == 0)
        .then(|| processes.first().cloned())
        .flatten();
    (tracked, running)
}

/// Every visible class with a guard was checked, has the flags [`sim_check`] gives its path,
/// and is protected when git tracks it. Returns the classes flagged tracked and in use.
pub(crate) fn check_pool_flags(model: &Model, rules: &RuleSet) -> Result<(u64, u64), String> {
    let tree = &model.tree;
    let (mut tracked, mut in_use) = (0, 0);
    for (ix, c) in tree.classes().filter(|(ix, _)| tree.is_visible(*ix)) {
        let g = &rules.rule(c.rule).guard;
        if !g.not_vcs_tracked && g.not_running.is_empty() {
            continue;
        }
        // Hashed as the request had it.
        let path = tree.path_buf(ix);
        let show = || format!("{} ({c:?})", path.display());
        if !c.has(Class::CHECKED) || c.has(Class::QUEUED) {
            return Err(format!("{}: not checked", show()));
        }
        let (t, r) = pool_answer(
            path.as_os_str().as_encoded_bytes(),
            g.not_vcs_tracked,
            &g.not_running,
        );
        let t = t == Some(true);
        if c.has(Class::VCS_TRACKED) != t || c.has(Class::IN_USE) != r.is_some() {
            return Err(format!(
                "{}: flags do not match the answer for its path",
                show()
            ));
        }
        if t && c.safety != dfdu_rules::Safety::Protected {
            return Err(format!("{}: git-tracked but not protected", show()));
        }
        tracked += u64::from(t);
        in_use += u64::from(r.is_some());
    }
    Ok((tracked, in_use))
}

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

/// Compares every directory in the final tree with the spec. Returns (dirs checked, abandoned).
#[expect(
    clippy::too_many_lines,
    reason = "one walk comparing each directory with the spec, its checks kept together"
)]
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
    let state_of = |p: &[u8]| by_path.get(p).and_then(|&ix| tree.dir(ix)).map(|d| d.state);

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
    // Which spec entries the scan saw, and the lowest path of every hard-linked inode.
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
                    e.clone_from(&path);
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
        abandoned += u64::from(state == DirState::Abandoned);
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
            t.mounts = u64::from(mount_root(*d));
            // Set when the directory was opened; check_subvol verified the flag.
            t.ro_subvols = u64::from(
                by_path
                    .get(path)
                    .is_some_and(|&ix| tree.flags(ix).has(NodeFlags::RO_SUBVOL)),
            );
            if let Some(s) = state_of(path)
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
                .filter(|p| is_child(p, path))
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
    Ok((checked, abandoned))
}
