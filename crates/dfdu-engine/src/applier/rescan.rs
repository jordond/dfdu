//! Rescans of visible directories, and resuming a tree loaded from a file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, DirId, JobId, JobOptions};
use dfdu_tree::{DirState, Ix, NodeFlags, NodeRef, Pages, Tree};

use super::jobs::JobStart;
use super::{Applier, Job, JobKind, JobPhase, Model, meta_of};
use crate::links::VISIBLE;

/// What a rescan of directory `ix` classifies against: the rules decided above it, whether it
/// is protected, and its own hits, from its parent's listing as the tree has it. `job` is what
/// the job that scanned `ix` classified against, and `job_root` that job's root.
fn rescan_class(tree: &Tree, ix: Ix, job_root: Option<Ix>, job: &ClassOpts) -> ClassOpts {
    let rules = job.rules.clone();
    // The rule path: the job root's, then the names below it.
    let mut names = Vec::new();
    let mut cur = Some(ix);
    while let Some(i) = cur
        && Some(i) != job_root
    {
        names.push(tree.name(i));
        cur = tree.parent(i);
    }
    let path_of = |names: &[&[u8]]| {
        let mut p = job.path.clone();
        for n in names.iter().rev() {
            if !p.ends_with(b"/") {
                p.push(b'/');
            }
            p.extend_from_slice(n);
        }
        p
    };
    let path = if cur.is_some() {
        path_of(&names)
    } else {
        dfdu_rules::rule_path(&tree.path_buf(ix))
    };
    let mut inside = Vec::new();
    let mut cur = tree.parent(ix);
    while let Some(p) = cur {
        inside.extend(tree.class(p).map(|c| c.rule));
        cur = tree.parent(p);
    }
    inside.reverse();
    let protection = |i: Ix| {
        if tree.is_protected(i) {
            dfdu_rules::protected::State::On
        } else {
            dfdu_rules::protected::State::Off
        }
    };
    let mut chain = Vec::new();
    if let Some(p) = tree.parent(ix) {
        let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
        let parent = if cut == 0 {
            b"/".to_vec()
        } else {
            path[..cut].to_vec()
        };
        let sibling = |n: &[u8]| tree.children(p).any(|c| tree.name(c) == n);
        let v = rules.classify(&dfdu_rules::Candidate {
            parent: &parent,
            name: tree.name(ix),
            is_dir: true,
            sibling: &sibling,
            inside: &inside,
            protection: protection(p),
            partial: false,
            stat: None,
            link: None,
        });
        if let dfdu_rules::Verdict::Hits(h) = v {
            chain = h;
        }
    }
    ClassOpts {
        rules,
        path,
        inside,
        protection: protection(ix),
        chain,
    }
}

/// More changed directories than this and a resume rescans the whole tree.
const MAX_RESUME_RESCANS: usize = 10_000;

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

/// Whether `ix` is a mount the scan listed but did not enter: not a kernel filesystem, which is
/// never scanned, nor a second mount of something counted elsewhere.
fn left_out(tree: &Tree, ix: Ix) -> bool {
    let f = tree.flags(ix);
    tree.dir(ix).is_some_and(|d| d.state == DirState::Skipped)
        && f.has(NodeFlags::MOUNT_ROOT)
        && !f.has(NodeFlags::DUP_MOUNT)
        && !f.has(NodeFlags::KERNEL)
}

impl<B: Backend> Applier<B> {
    /// Adds `entered` (a mount root's device and inode) to the mounts entered this session,
    /// and has `opts` enter all of them.
    fn enter_again(&mut self, opts: &mut JobOptions, entered: Option<(u64, u64)>) {
        if let Some(at) = entered.filter(|at| !self.entered.contains(at)) {
            self.entered.push(at);
        }
        for &at in &self.entered {
            if !opts.enter.contains(&at) {
                opts.enter.push(at);
            }
        }
    }

    /// Rescans `node` with the options of the job that scanned it, or `given`. With `enter`,
    /// `node` is a mount that was not entered, and is scanned in place: a job's root is read
    /// whatever filesystem it is on. Every rescan enters the mounts entered so far.
    pub(super) fn rescan(
        &mut self,
        m: &mut Model,
        node: NodeRef,
        given: Option<JobOptions>,
        enter: bool,
    ) {
        let Some(ix) = m.tree.resolve_visible(node) else {
            m.status.notice("rescan: that directory is gone".into());
            return;
        };
        if enter && !left_out(&m.tree, ix) {
            m.status
                .notice("enter: that is not a mount the scan left out".into());
            return;
        }
        let owner_id = match m.tree.dir(ix) {
            Some(d) if enter => d.job.0,
            None => {
                m.status.notice("rescan: not a directory".into());
                return;
            }
            Some(_) if m.tree.flags(ix).has(NodeFlags::DUP_MOUNT) => {
                m.status
                    .notice("rescan: that directory is counted at another mount of it".into());
                return;
            }
            Some(_) if m.tree.flags(ix).has(NodeFlags::KERNEL) => {
                m.status
                    .notice("rescan: that is a kernel filesystem, which is never scanned".into());
                return;
            }
            Some(_) if m.tree.flags(ix).has(NodeFlags::NETWORK) => {
                m.status.notice(
                    "rescan: that is a network filesystem; start dfdu with --network to scan it"
                        .into(),
                );
                return;
            }
            Some(d) if d.state == DirState::Skipped => {
                m.status
                    .notice("rescan: that directory is on another filesystem".into());
                return;
            }
            Some(d) => d.job.0,
        };
        // Overlap with deletes and other rescans.
        if self.plan_overlaps(&m.tree, ix) {
            m.status.notice(
                "rescan: a delete in this directory is in progress; rescan after it completes"
                    .into(),
            );
            return;
        }
        if !self.supersede_rescans(m, ix) {
            return;
        }
        self.cut(m, ix, DirState::Superseded);
        let id = self.next_job;
        self.next_job += 1;
        let path = m.tree.path_buf(ix);
        let shadow = m.tree.new_root(
            m.tree.name(ix).to_vec().as_slice(),
            &meta_of(&m.tree, ix),
            false,
        );
        // Whether the directory is a mount root depends on its parent, which the rescan keeps.
        if m.tree.flags(ix).has(NodeFlags::MOUNT_ROOT) {
            m.tree.set_flag(shadow, NodeFlags::MOUNT_ROOT, true);
        }
        let owner = self.jobs.get(&owner_id);
        let owner_root = owner.and_then(|j| m.tree.resolve(j.root));
        let resumed = given.is_some();
        let mut opts = given
            .or_else(|| owner.map(|j| j.opts.clone()))
            .or_else(|| self.retired.get(&owner_id).cloned())
            .unwrap_or_default();
        // Duplicate mounts are judged against the whole visible tree, not just this part.
        let mut top = ix;
        while let Some(p) = m.tree.parent(top) {
            top = p;
        }
        opts.scope = Some(m.tree.path_buf(top));
        self.enter_again(&mut opts, enter.then(|| m.tree.dev_ino(ix)));
        if let Some(c) = opts.class.as_ref().filter(|_| resumed) {
            m.rules = Some(c.rules.clone());
            m.tree
                .set_protection(shadow, c.protection.protected(), false);
        } else if let Some(c) = &mut opts.class {
            *c = rescan_class(&m.tree, ix, owner_root, c);
            let f = m.tree.flags(ix);
            m.tree.set_protection(
                shadow,
                f.has(NodeFlags::PROTECTED),
                f.has(NodeFlags::EXCEPTED),
            );
        }
        let start = JobStart {
            kind: JobKind::Rescan,
            target: ix,
            root: shadow,
            scope: id,
            path,
            opts,
        };
        self.start(m, id, start);
    }

    /// Whether a rescan of `ix` may start: not while a rescan of it or of a parent waits to
    /// swap in. Rescans of its subdirectories are cancelled, the new one replacing them.
    fn supersede_rescans(&mut self, m: &mut Model, ix: Ix) -> bool {
        let mut cancel = Vec::new();
        for j in self
            .jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.swap_pending())
        {
            let Some(t) = m.tree.resolve_visible(j.target) else {
                continue;
            };
            if t == ix || Self::is_ancestor(&m.tree, t, ix) {
                m.status
                    .notice("rescan: already rescanning this directory or a parent".into());
                return false;
            }
            if Self::is_ancestor(&m.tree, ix, t) {
                cancel.push(j.id.0);
            }
        }
        for id in cancel {
            self.cancel(id);
        }
        true
    }

    /// Takes over a tree loaded from a snapshot when the volume's change journal says which
    /// directories changed since (`changed`, relative to the root): the tree is classified as
    /// it stands, held by a finished job with `opts`, and only the topmost changed directories
    /// are rescanned, each with its subtree. A changed root, or too many changes, rescan it all.
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
        let mut targets = changed_dirs(&m.tree, root, changed);
        if targets.len() > MAX_RESUME_RESCANS || targets.contains(&root) {
            return self.rescan(m, node, Some(opts), false);
        }
        let id = self.next_job;
        self.next_job += 1;
        // Other volumes mounted below keep their own journals: their mount roots are rescanned.
        // So are btrfs subvolumes below (another device, no mount): each has a tree, and a
        // generation, of its own.
        let mut mounts = Vec::new();
        let mut own = |tree: &mut Tree, ix: Ix| {
            if let Some(d) = tree.dir_mut(ix) {
                d.job = JobId(id);
                d.dir_id = DirId::ROOT;
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
        targets.extend(mounts);
        let targets = topmost(&m.tree, targets);
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
            },
        );
        m.tree.bump_version();
        self.queue_checks(m);
        for t in targets {
            let r = m.tree.node_ref(t);
            self.rescan(m, r, None, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfdu_tree::{Kind, Meta};

    fn meta(kind: Kind, ino: u64) -> Meta {
        Meta {
            kind,
            apparent: 0,
            alloc: 0,
            mtime: 0,
            dev: 1,
            ino,
            nlink: 1,
            mount: 0,
        }
    }

    /// /r/{a/{b/, f}, c/}; returns the tree and r, a, b, c.
    #[expect(
        clippy::many_single_char_names,
        reason = "nodes named as in the fixture's diagram"
    )]
    fn tree() -> (Tree, [Ix; 4]) {
        let mut t = Tree::new();
        let r = t.new_root(b"/r", &meta(Kind::Dir, 1), true);
        let a = t.add_dir(r, b"a", &meta(Kind::Dir, 2));
        let b = t.add_dir(a, b"b", &meta(Kind::Dir, 3));
        let c = t.add_dir(r, b"c", &meta(Kind::Dir, 4));
        t.add_file(a, b"f", &meta(Kind::File, 5), false, None);
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
}
