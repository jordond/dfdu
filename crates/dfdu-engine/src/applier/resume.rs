//! Resuming a tree loaded from a file with what the volume's change journal says changed: a
//! journal names the directories whose own entries changed, so each is listed again and the
//! subdirectories it still has are kept as loaded.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dfdu_scan::journal::Keep;
use dfdu_scan::{Backend, DirId, JobId, JobOptions};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Pages, Tree};

use super::work::Work;
use super::{Applier, Job, JobKind, JobPhase, Model};
use crate::links::VISIBLE;

/// What a resume's rescan keeps from the loaded tree: the loaded directories by device and
/// inode, which its scanner does not read ([`Keep`]), and the placeholders it added for those
/// it met, which the loaded directories replace when it swaps in.
#[derive(Default)]
pub(super) struct Kept {
    nodes: HashMap<(u64, u64), NodeRef>,
    pub placeholders: Vec<NodeRef>,
}

impl Kept {
    /// What its scanner is told not to read.
    pub(super) fn keep(&self) -> Keep {
        Keep::new(self.nodes.keys().copied())
    }
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
                k.nodes.insert(tree.dev_ino(c), tree.node_ref(c));
            }
        }
    }
    out
}

impl<B: Backend> Applier<B> {
    /// Takes over a tree loaded from a snapshot when the volume's change journal says which
    /// directories changed since (`changed`, relative to the root): the tree is classified as
    /// it stands and held by a finished job with `opts`. Each changed directory, and every
    /// directory above it, is listed again by a rescan of the topmost: the subdirectories
    /// those listings find that the loaded tree has (by device and inode) are kept as loaded
    /// unless listed again themselves, and new ones are scanned in full. Volumes mounted below
    /// keep journals of their own (or none): their mount roots are scanned again in full.
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
        let id = self.next_job;
        self.next_job += 1;
        // Mounts below keep journals of their own, or none, and so do btrfs subvolumes below
        // (another device, no mount), each with a tree and a generation of its own: they are
        // scanned again in full.
        let mut mounts = Vec::new();
        let links = &mut self.links;
        let mut own = |tree: &mut Tree, ix: Ix| {
            if let Some(d) = tree.dir_mut(ix) {
                d.job = JobId(id);
                d.dir_id = DirId::ROOT;
            }
            // Its hard links are the visible ones, which the links rescans find are merged
            // with; one of each inode is charged, the lowest path once the rescans end.
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
            let other_dev = || {
                tree.parent(ix)
                    .is_some_and(|p| tree.dev_ino(p).0 != tree.dev_ino(ix).0)
            };
            if ix != root
                && (tree.flags(ix).has(NodeFlags::MOUNT_ROOT) || other_dev())
                && tree.dir(ix).is_some_and(|d| d.state != DirState::Skipped)
            {
                mounts.push(ix);
            }
        };
        if let Some(c) = &opts.class {
            m.rules = Some(c.rules.clone());
            m.tree.set_protection(root, c.protection.protected(), false);
            crate::reclassify::classify_loaded(&mut m.tree, root, c, &self.scan.backend, &mut own);
        } else {
            let mut stack = vec![root];
            while let Some(ix) = stack.pop() {
                own(&mut m.tree, ix);
                stack.extend(m.tree.children(ix).filter(|&c| m.tree.dir(c).is_some()));
            }
        }
        // Snapshots keep no measured extents: the loaded files are measured again where files
        // can share them (rescanned parts are measured when their rescans end).
        let mut devs = vec![root];
        devs.extend(mounts.iter().copied());
        for ix in devs {
            let dev = m.tree.dev_ino(ix).0;
            if !self.extent_devs.contains(&dev) {
                let loc = self.scan.backend.loc_from_path(&m.tree.path_buf(ix));
                if self.scan.backend.shares_extents(&loc) {
                    self.extent_devs.insert(dev);
                }
            }
        }
        self.start_measure(m, node);
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
        m.tree.bump_version();
        self.work.push_back(Work::FinalPassVisible);
        self.queue_checks(m);
        for (t, k) in targets.into_iter().zip(kept) {
            let r = m.tree.node_ref(t);
            self.rescan_keeping(m, r, None, false, k);
        }
    }

    /// Puts the loaded directories `kept` keeps in the place of their placeholders in the
    /// shadow of the rescan of `target`, which swaps in next. Returns what is rescanned once it
    /// has: a placeholder whose loaded directory is gone, and a kept directory found under
    /// another name or parent, or that the new listing classifies otherwise, since its subtree
    /// was classified where it was.
    pub(super) fn graft_kept(m: &mut Model, kept: &Kept, target: Ix) -> Vec<NodeRef> {
        let mut again = Vec::new();
        for &p in &kept.placeholders {
            let Some(px) = m.tree.resolve(p) else {
                continue;
            };
            let found = kept
                .nodes
                .get(&m.tree.dev_ino(px))
                .and_then(|&r| m.tree.resolve(r))
                .filter(|&kx| Self::is_ancestor(&m.tree, target, kx));
            let Some(kx) = found else {
                if let Some(d) = m.tree.dir_mut(px) {
                    d.state = DirState::Cancelled;
                    d.outcome = DirState::Cancelled;
                }
                again.push(p);
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
            m.tree.graft(px, kx);
            if moved {
                again.push(m.tree.node_ref(kx));
            }
        }
        again
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::meta;
    use dfdu_tree::Kind;

    /// /r/{a/{b/, f}, c/}; returns the tree and r, a, b, c.
    #[expect(
        clippy::many_single_char_names,
        reason = "nodes named as in the fixture's diagram"
    )]
    fn tree() -> (Tree, [Ix; 4]) {
        let mut t = Tree::new();
        let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
        let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 3));
        let c = t.add_dir(r, b"c", &meta(Kind::Dir, 0, 4));
        t.add_file(a, b"f", &meta(Kind::File, 0, 5), false, None);
        for d in [r, a, b, c] {
            t.dir_mut(d).unwrap().state = DirState::Complete;
        }
        (t, [r, a, b, c])
    }

    #[test]
    #[expect(
        clippy::many_single_char_names,
        reason = "nodes named as in the fixture's diagram"
    )]
    fn changed_paths_name_their_directory_or_its_nearest_ancestor() {
        let (t, [r, a, b, c]) = tree();
        let paths = ["a/b", "c", "a/gone/deeper", "a/f", ""].map(PathBuf::from);
        assert_eq!(changed_dirs(&t, r, &paths), [b, c, a, a, r]);
    }

    #[test]
    fn only_the_topmost_changed_directories_are_kept() {
        let (t, [_, a, b, c]) = tree();
        let mut want = vec![a, c];
        want.sort_unstable_by_key(|d| d.0);
        assert_eq!(topmost(&t, vec![b, c, a, b]), want);
    }

    /// A change in `b` lists `r`, `a` and `b` again, keeping `c`: the rescan is of the root
    /// when it changed, of `b` alone when it did not.
    #[test]
    fn what_is_listed_again_keeps_the_rest() {
        let (t, [r, a, b, c]) = tree();
        let keys = |k: &Option<Box<Kept>>| {
            let mut v: Vec<(u64, u64)> = k.as_ref().unwrap().nodes.keys().copied().collect();
            v.sort_unstable();
            v
        };
        let kept = kept_by_target(&t, r, &[r, b], &[r], &[]);
        assert_eq!(keys(&kept[0]), [t.dev_ino(c)]);
        let kept = kept_by_target(&t, r, &[a], &[a], &[]);
        assert_eq!(keys(&kept[0]), [t.dev_ino(b)]);
        let kept = kept_by_target(&t, r, &[b], &[b], &[]);
        assert!(keys(&kept[0]).is_empty());
        // A mount is scanned in full.
        assert!(kept_by_target(&t, r, &[c], &[c], &[c])[0].is_none());
    }

    /// A directory whose listing was not read to the end is listed again.
    #[test]
    fn an_incomplete_directory_is_not_kept() {
        let (mut t, [r, a, _, c]) = tree();
        t.dir_mut(c).unwrap().state = DirState::Partial;
        let kept = kept_by_target(&t, r, &[r], &[r], &[]);
        let keys: Vec<_> = kept[0].as_ref().unwrap().nodes.keys().copied().collect();
        assert_eq!(keys, [t.dev_ino(a)]);
    }
}
