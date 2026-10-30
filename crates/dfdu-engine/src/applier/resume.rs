//! Resuming a tree loaded from a file with what the volume's change journal says changed: a
//! journal names the directories whose own entries changed, so each is listed again and the
//! subdirectories it still has are kept as loaded.
//!
//! The resume first walks the loaded tree a step at a time ([`Work::Resume`]): it classifies
//! it, takes it over for a job of this session and registers its hard links. Until the walk
//! ends, nothing in the tree is deleted, rescanned or cleaned. Then the changed directories
//! are listed again by rescans that keep the loaded subdirectories they find.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dfdu_scan::classify::ClassOpts;
use dfdu_scan::journal::Keep;
use dfdu_scan::{Backend, DirId, JobId, JobOptions};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Pages, Tree};

use super::work::{STEP_UNITS, Work};
use super::{Applier, Command, Job, JobKind, JobPhase, Model};
use crate::links::{LinkTable, VISIBLE};
use crate::reclassify::ClassifyLoaded;

/// What a resume's rescan keeps from the loaded tree: the loaded directories by device and
/// inode, with the mtime each was loaded with, which its scanner does not read while the mtime
/// holds ([`Keep`]), and the placeholders it added for those it met, which the loaded
/// directories replace when it swaps in.
#[derive(Default)]
pub(super) struct Kept {
    nodes: HashMap<(u64, u64), (NodeRef, i64)>,
    pub placeholders: Vec<NodeRef>,
}

impl Kept {
    /// What its scanner is told not to read.
    pub(super) fn keep(&self) -> Keep {
        Keep::new(self.nodes.iter().map(|(&k, &(_, mtime))| (k, mtime)))
    }
}

/// A resume's walk over the loaded tree, a step at a time: the directories still to take over,
/// and what the rescans that follow it need.
pub(super) struct ResumeWalk {
    /// The resume's job, which holds the loaded tree.
    id: u32,
    /// The loaded root.
    root: NodeRef,
    /// The directories to list again.
    wanted: Vec<NodeRef>,
    /// Mount roots and subvolumes met so far, scanned again in full.
    mounts: Vec<NodeRef>,
    /// The classification, when the job classifies, which takes the walk's directories in turn.
    class: Option<(ClassOpts, ClassifyLoaded)>,
    /// The directories still to take over when it does not.
    stack: Vec<NodeRef>,
    /// Cancelled while it walked: its rescans are cancelled as they start.
    cancelled: bool,
}

/// Whether `ix` was loaded from a file ([`Engine::load`](crate::Engine::load)) and no resume has
/// taken it over yet: its directory (a file's parent) belongs to no job of this session. Until
/// one has, the tree may be out of date anywhere below `ix`, which the identity the deleter
/// checks again does not show. Deletes and native commands refuse such nodes.
///
/// What a resume guarantees once it has: a directory listed again is as it is now; one it
/// kept as loaded is trusted on the journal's word that its entries did not change, and, when
/// its parent was listed again, on its own mtime being the one it was loaded with. A change
/// deeper below a kept directory that the journal lost is seen only by a rescan of it (the
/// cache's periodic full scan is one).
#[must_use]
pub fn unrefreshed(tree: &Tree, ix: Ix) -> bool {
    let dir = if tree.dir(ix).is_some() {
        Some(ix)
    } else {
        tree.parent(ix)
    };
    dir.and_then(|d| tree.dir(d))
        .is_some_and(|d| d.job == JobId(0))
}

/// The directories of the tree at `root` that `paths` (relative to it) name, or the nearest
/// existing ancestor of one not in the tree.
fn changed_dirs(tree: &Tree, root: Ix, paths: &[PathBuf]) -> Vec<Ix> {
    let mut out = Vec::new();
    for p in paths {
        let mut at = root;
        for c in p.components() {
            let std::path::Component::Normal(n) = c else {
                continue;
            };
            let n = dfdu_tree::name_bytes(n);
            match tree
                .children(at)
                .find(|&k| tree.dir(k).is_some() && tree.name(k) == &n[..])
            {
                Some(k) => at = k,
                None => break,
            }
        }
        out.push(at);
    }
    out
}

/// `dirs` without duplicates and without those below another of them.
fn topmost(tree: &Tree, mut dirs: Vec<Ix>) -> Vec<Ix> {
    dirs.sort_unstable_by_key(|d| d.0);
    dirs.dedup();
    let set: HashSet<Ix> = dirs.iter().copied().collect();
    dirs.retain(|&d| {
        let mut cur = tree.parent(d);
        while let Some(p) = cur {
            if set.contains(&p) {
                return false;
            }
            cur = tree.parent(p);
        }
        true
    });
    dirs
}

/// What the rescan of each of `targets` (the topmost of `wanted`) keeps: of the directories
/// it lists again (those of `wanted` and every one above them up to the target), the loaded
/// subdirectories that are not listed again themselves, were read to the end, and are on the
/// root's volume and no mount. Nothing for a target in `mounts`, which is scanned in full.
fn kept_by_target(
    tree: &Tree,
    root: Ix,
    wanted: &[Ix],
    targets: &[Ix],
    mounts: &[Ix],
) -> Vec<Option<Box<Kept>>> {
    let dev = tree.dev_ino(root).0;
    let at: HashMap<Ix, usize> = targets.iter().enumerate().map(|(i, &t)| (t, i)).collect();
    // Each directory listed again, with the index of its target.
    let mut listed: HashMap<Ix, usize> = HashMap::new();
    for &d in wanted {
        let mut path = Vec::new();
        let mut cur = d;
        let target = loop {
            if let Some(&i) = listed.get(&cur).or_else(|| at.get(&cur)) {
                break Some(i);
            }
            path.push(cur);
            match tree.parent(cur) {
                Some(p) => cur = p,
                None => break None,
            }
        };
        if let Some(i) = target {
            listed.insert(cur, i);
            listed.extend(path.into_iter().map(|p| (p, i)));
        }
    }
    let mounts: HashSet<Ix> = mounts.iter().copied().collect();
    let mut out: Vec<Option<Box<Kept>>> = targets
        .iter()
        .map(|t| (!mounts.contains(t)).then(Box::default))
        .collect();
    for (&l, &i) in &listed {
        let Some(k) = out[i].as_mut().filter(|_| tree.dev_ino(l).0 == dev) else {
            continue;
        };
        for c in tree.children(l) {
            let keep = tree.dir(c).is_some_and(|d| d.state == DirState::Complete)
                && !listed.contains_key(&c)
                && tree.dev_ino(c).0 == dev
                && !tree.flags(c).has(NodeFlags::MOUNT_ROOT);
            if keep {
                k.nodes
                    .insert(tree.dev_ino(c), (tree.node_ref(c), tree.mtime(c)));
            }
        }
    }
    out
}

/// Takes loaded directory `ix` over for job `id`: its hard links are registered and, below
/// `root`, a mount root or subvolume is added to `mounts`.
fn own(
    tree: &mut Tree,
    links: &mut LinkTable,
    id: u32,
    root: Ix,
    ix: Ix,
    mounts: &mut Vec<NodeRef>,
) {
    if let Some(d) = tree.dir_mut(ix) {
        d.job = JobId(id);
        d.dir_id = DirId::ROOT;
    }
    // Its hard links are the visible ones, which the links rescans find are merged with; one
    // of each inode is charged, the lowest path once the rescans end.
    let linked: Vec<Ix> = tree
        .children(ix)
        .filter(|&c| tree.flags(c).has(NodeFlags::HARDLINK))
        .collect();
    for c in linked {
        links.add_scanned(tree.dev_ino(c), 0, VISIBLE, |charged| {
            tree.set_charged(c, charged);
            tree.node_ref(c)
        });
    }
    // Mounts below keep journals of their own, or none, and so do btrfs subvolumes below
    // (another device, no mount), each with a tree and a generation of its own: they are
    // scanned again in full.
    let other_dev = tree
        .parent(ix)
        .is_some_and(|p| tree.dev_ino(p).0 != tree.dev_ino(ix).0);
    if ix != root
        && (tree.flags(ix).has(NodeFlags::MOUNT_ROOT) || other_dev)
        && tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped)
    {
        mounts.push(tree.node_ref(ix));
    }
}

/// Whether the class `kx` was loaded with still holds where it is now in the visible tree, as
/// far as its parent's listing decides: its rule is among the hits the listing gives it. A
/// rule that looks inside `kx` is decided by its loaded entries, which the listing does not
/// read, so a hit is all it can say; one whose sibling condition no longer holds (a marker
/// file removed) gives none.
fn class_holds(tree: &Tree, kx: Ix, class: &ClassOpts) -> bool {
    let Some(rule) = tree.class(kx).map(|c| c.rule) else {
        return true;
    };
    super::rescan::rescan_class(tree, kx, None, class)
        .chain
        .iter()
        .any(|h| h.rule == rule)
}

/// What [`Applier::graft_kept`] leaves to do once the rescan swapped in.
pub(super) struct Grafted {
    /// Rescanned as they are.
    again: Vec<NodeRef>,
    /// Kept directories classified as loaded that the new listing did not classify: rescanned
    /// unless their class still holds ([`class_holds`]).
    check: Vec<NodeRef>,
}

impl Grafted {
    /// The kept directories to rescan, judged in the visible tree against `class`, what the
    /// rescan classified against.
    pub(super) fn rescans(self, tree: &Tree, class: Option<&ClassOpts>) -> Vec<NodeRef> {
        let mut out = self.again;
        out.extend(
            self.check
                .into_iter()
                .filter(|&r| match (tree.resolve_visible(r), class) {
                    (Some(kx), Some(c)) => !class_holds(tree, kx, c),
                    _ => false,
                }),
        );
        out
    }
}

impl<B: Backend> Applier<B> {
    /// Takes over a tree loaded from a snapshot when the volume's change journal says which
    /// directories changed since (`changed`, relative to the root): the tree is classified as
    /// it stands and held by a finished job with `opts`, a step at a time ([`Work::Resume`]).
    /// Each changed directory, and every directory above it, is then listed again by a rescan
    /// of the topmost: the subdirectories those listings find that the loaded tree has (by
    /// device and inode, with the mtime it was loaded with) are kept as loaded unless listed
    /// again themselves, and new ones are scanned in full. The root is listed again when its
    /// own mtime changed, whatever the journal says, and scanned again in full when it was
    /// replaced. Volumes mounted below keep journals of their own (or none): their mount roots
    /// are scanned again in full.
    pub(super) fn resume_changed(
        &mut self,
        m: &mut Model,
        node: NodeRef,
        opts: JobOptions,
        changed: &[PathBuf],
    ) {
        let Some(root) = m
            .tree
            .resolve_visible(node)
            .filter(|&r| m.tree.dir(r).is_some() && m.tree.parent(r).is_none())
        else {
            return self.rescan(m, node, Some(opts), false);
        };
        let mut wanted = changed_dirs(&m.tree, root, changed);
        if self.remote.is_none() {
            let loc = self.scan.backend.loc_from_path(&m.tree.path_buf(root));
            match self.scan.backend.stat_root(&loc) {
                Ok(now) if (now.dev, now.ino) != m.tree.dev_ino(root) => {
                    return self.rescan(m, node, Some(opts), false);
                }
                Ok(now) if now.mtime == m.tree.mtime(root) => {}
                // Changed, or unreadable: its rescan says why.
                _ => wanted.push(root),
            }
        }
        let id = self.next_job;
        self.next_job += 1;
        if let Some(c) = &opts.class {
            m.rules = Some(c.rules.clone());
            m.tree.set_protection(root, c.protection.protected(), false);
        }
        let class = opts
            .class
            .clone()
            .map(|c| (c.clone(), ClassifyLoaded::new(&m.tree, root, &c)));
        let stack = if class.is_some() { vec![] } else { vec![node] };
        self.jobs.insert(
            id,
            Job {
                id: JobId(id),
                kind: JobKind::Initial,
                target: node,
                root: node,
                scope: VISIBLE,
                opts,
                map: Pages::default(),
                held_close: HashMap::new(),
                stalled: HashMap::new(),
                submitted_seq: 0,
                phase: JobPhase::Done,
                measures: false,
                kept: None,
            },
        );
        let wanted = wanted.into_iter().map(|w| m.tree.node_ref(w)).collect();
        self.work.push_back(Work::Resume(Box::new(ResumeWalk {
            id,
            root: node,
            wanted,
            mounts: Vec::new(),
            class,
            stack,
            cancelled: false,
        })));
    }

    /// One step of a resume's walk; once it has taken the whole tree over, starts its rescans.
    pub(super) fn resume_step(&mut self, m: &mut Model, mut walk: Box<ResumeWalk>) {
        let Some(root) = m.tree.resolve_visible(walk.root) else {
            return;
        };
        let ResumeWalk {
            id,
            mounts,
            class,
            stack,
            ..
        } = &mut *walk;
        let (id, links) = (*id, &mut self.links);
        let done = match class {
            Some((c, w)) => w.step(&mut m.tree, c, &self.scan.backend, STEP_UNITS, |t, ix| {
                own(t, links, id, root, ix, mounts);
            }),
            None => {
                let mut n = 0;
                while n < STEP_UNITS
                    && let Some(r) = stack.pop()
                {
                    let Some(ix) = m.tree.resolve_visible(r) else {
                        continue;
                    };
                    own(&mut m.tree, links, id, root, ix, mounts);
                    let t = &m.tree;
                    n += 1 + t.children(ix).count();
                    stack.extend(
                        t.children(ix)
                            .filter(|&c| t.dir(c).is_some())
                            .map(|c| t.node_ref(c)),
                    );
                }
                stack.is_empty()
            }
        };
        if done {
            self.resume_rescans(m, root, &walk);
        } else {
            self.work.push_front(Work::Resume(walk));
        }
    }

    /// Ends a resume's walk: the loaded files are measured where files can share extents, and
    /// the changed directories and the mounts below are listed again.
    fn resume_rescans(&mut self, m: &mut Model, root: Ix, walk: &ResumeWalk) {
        let tree = &m.tree;
        let mounts: Vec<Ix> = walk
            .mounts
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        let mut wanted: Vec<Ix> = walk
            .wanted
            .iter()
            .filter_map(|&r| tree.resolve_visible(r))
            .collect();
        // Snapshots keep no measured extents: the loaded files are measured again where files
        // can share them (rescanned parts are measured when their rescans end).
        for ix in std::iter::once(root).chain(mounts.iter().copied()) {
            let dev = m.tree.dev_ino(ix).0;
            if !self.extent_devs.contains(&dev) {
                let loc = self.scan.backend.loc_from_path(&m.tree.path_buf(ix));
                if self.scan.backend.shares_extents(&loc) {
                    self.extent_devs.insert(dev);
                }
            }
        }
        self.start_measure(m, walk.root);
        // The loaded tree entered them: so do the rescans whose listings meet them.
        for &ix in &mounts {
            let at = m.tree.dev_ino(ix);
            if !self.entered.contains(&at) {
                self.entered.push(at);
            }
        }
        wanted.extend(mounts.iter().copied());
        let targets = topmost(&m.tree, wanted.clone());
        let kept = kept_by_target(&m.tree, root, &wanted, &targets, &mounts);
        m.tree.bump_version();
        self.work.push_back(Work::FinalPassVisible);
        self.queue_checks(m);
        for (t, k) in targets.into_iter().zip(kept) {
            let r = m.tree.node_ref(t);
            let first = self.next_job;
            self.rescan_keeping(m, r, None, false, k);
            if walk.cancelled {
                for id in first..self.next_job {
                    self.cancel(id);
                }
            }
        }
    }

    /// Whether a resume's walk is still going.
    fn resuming(&self) -> bool {
        self.work.iter().any(|w| matches!(w, Work::Resume(_)))
    }

    /// Marks the resume walking as cancelled, if `job` is its job (all when `None`).
    pub(super) fn cancel_resume(&mut self, job: Option<u32>) {
        for w in &mut self.work {
            if let Work::Resume(walk) = w
                && job.is_none_or(|j| j == walk.id)
            {
                walk.cancelled = true;
            }
        }
    }

    /// Why `cmd` is refused on a loaded tree, if it is: everything that would touch the tree
    /// while a resume walks it, and a native command on what no resume has taken over yet
    /// (deletes check each item, [`unrefreshed`]).
    pub(super) fn resume_refusal(&self, m: &Model, cmd: &Command) -> Option<&'static str> {
        let touches = matches!(
            cmd,
            Command::Rescan { .. }
                | Command::Enter { .. }
                | Command::Resume { .. }
                | Command::Delete { .. }
                | Command::RunNative { dry_run: false, .. }
        );
        if touches && self.resuming() {
            return Some(
                "not yet: the loaded tree is still being taken over; try again in a moment",
            );
        }
        if let Command::RunNative {
            node,
            dry_run: false,
            ..
        } = cmd
            && m.tree
                .resolve_visible(*node)
                .is_some_and(|ix| unrefreshed(&m.tree, ix))
        {
            return Some(
                "native command: not refreshed yet: loaded from a file, and no resume has taken it over",
            );
        }
        None
    }

    /// Puts the loaded directories `kept` keeps in the place of their placeholders in the
    /// shadow of the rescan of `target`, which swaps in next. What is rescanned once it has: a
    /// placeholder whose loaded directory is gone, a kept directory found under another name
    /// or parent, or that the new listing classifies otherwise, since its subtree was
    /// classified where it was ([`Grafted`]).
    pub(super) fn graft_kept(m: &mut Model, kept: &Kept, target: Ix) -> Grafted {
        let mut out = Grafted {
            again: Vec::new(),
            check: Vec::new(),
        };
        for &p in &kept.placeholders {
            let Some(px) = m.tree.resolve(p) else {
                continue;
            };
            let found = kept
                .nodes
                .get(&m.tree.dev_ino(px))
                .and_then(|&(r, _)| m.tree.resolve(r))
                .filter(|&kx| Self::is_ancestor(&m.tree, target, kx));
            let Some(kx) = found else {
                if let Some(d) = m.tree.dir_mut(px) {
                    d.state = DirState::Cancelled;
                    d.outcome = DirState::Cancelled;
                }
                out.again.push(p);
                continue;
            };
            let t = &m.tree;
            let parent = |ix: Ix| t.parent(ix).map(|q| t.dev_ino(q));
            let rule = |ix: Ix| t.class(ix).map(|c| c.rule);
            let marks = |ix: Ix| {
                let f = t.flags(ix);
                (f.has(NodeFlags::PROTECTED), f.has(NodeFlags::EXCEPTED))
            };
            let moved = t.name(px) != t.name(kx)
                || parent(px) != parent(kx)
                || rule(px).is_some_and(|r| rule(kx) != Some(r))
                || marks(px) != marks(kx);
            // Not classified by the listing, but as loaded: the listing cannot decide a rule
            // that looks inside, so whether it still holds is judged once it swapped in.
            let unsure = !moved && rule(px).is_none() && rule(kx).is_some();
            m.tree.graft(px, kx);
            if moved {
                out.again.push(m.tree.node_ref(kx));
            } else if unsure {
                out.check.push(m.tree.node_ref(kx));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
