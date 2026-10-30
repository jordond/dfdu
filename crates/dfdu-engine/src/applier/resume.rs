//! Resuming a tree loaded from a file with what the volume's change journal says changed: a
//! journal names the directories whose own entries changed, so each is listed again and the
//! subdirectories it still has are kept as loaded.
//!
//! The resume first walks the loaded tree a step at a time ([`Work::Resume`]): it classifies
//! it, takes it over for a job of this session and registers its hard links. Until the walk
//! ends, nothing in the tree is deleted, rescanned or cleaned. Then the changed directories
//! are listed again by rescans that keep the loaded subdirectories they find.

mod graft;
mod walk;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dfdu_scan::journal::Keep;
use dfdu_scan::{Backend, JobId, JobOptions};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Pages, Tree};

use super::work::Work;
use super::{Applier, Command, Job, JobKind, JobPhase, Model};
use crate::links::VISIBLE;
use crate::reclassify::ClassifyLoaded;
pub(in crate::applier) use walk::ResumeWalk;

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

/// Whether `ix` was loaded from a file ([`Engine::load`](crate::Engine::load)) and no resume has
/// taken it over yet: its directory (a file's parent) belongs to no job of this session. Until
/// one has, the tree may be out of date anywhere below `ix`, which the identity the deleter
/// checks again does not show. Deletes and native commands refuse such nodes.
///
/// What a resume guarantees once it has: nothing is deleted while it walks the tree, nor in a
/// directory one of its rescans is listing again; a directory listed again is as it is now; one
/// it kept as loaded is trusted on the journal's word that its entries did not change, and, when
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
        self.work.push_back(Work::Resume(Box::new(ResumeWalk::new(
            id, node, wanted, class, stack,
        ))));
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
}

#[cfg(test)]
mod tests;
