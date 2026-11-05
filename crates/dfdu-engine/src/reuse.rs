//! A scan of a folder above the root of another scan of this machine (up past the root, or a
//! folder gone to above it) that keeps what that scan read: the old scan's tree below its root
//! is copied out under its read lock as a snapshot would load it ([`Scanned`]), and the new
//! scan starts from a tree of the folder with that copy below it ([`graft`]), which a resume
//! brings up to date ([`Command::Resume`](crate::Command::Resume)). The folder and those
//! between it and the old root are listed again; each directory of the copy is kept when its
//! stat is the one it was read with, and listed again otherwise, as a cached scan's are.
//!
//! The old scan may still be running. Its directories not read to the end yet are listed again
//! by the new scan ([`Scanned::unfinished`]), keeping those below them that were.
//!
//! The copy is a loaded tree: no classes (the resume classifies it with the new root's rules
//! and protection), no jobs, no measured extents, and a hard link charged where it is first met
//! (the resume elects one of each inode again, with those the rest of the folder holds).

use std::collections::HashSet;
use std::path::PathBuf;

use dfdu_scan::journal::JournalPosition;
use dfdu_tree::{DirState, HostPath, Ix, Kind, Meta, NodeFlags, Tree};

use crate::applier::Model;

/// The tree a scan of this machine read below its root, copied out for a scan of a folder
/// above it ([`Engine::scanned`](crate::Engine::scanned)).
pub struct Scanned {
    /// A visible root, named by its path, with what was below it.
    tree: Tree,
    /// Seconds since the Unix epoch before the scan read any of it: a directory whose mtime is
    /// not older may have changed in that same second after it was read, so it is listed again
    /// whatever its stat says.
    pub since: u64,
    /// Seconds since the Unix epoch when what it holds was last read in full: `since`, or
    /// earlier when the scan kept a cached scan's directories.
    pub full_scan: u64,
    /// Where the change journal stood before the scan read any of it, when its caller took
    /// the position (the session's cache link); `None` otherwise. A scan that keeps the copy
    /// is saved with it, so that what changed since the copy was read is asked about again.
    pub journal: Option<JournalPosition>,
    /// How long the scan that read it took, in milliseconds, when its caller knows; 0
    /// otherwise.
    pub scan_ms: u64,
    /// The directories the old scan had not read to the end when it was copied, relative to
    /// its root (empty for the root itself): the new scan lists them again. Empty for a
    /// finished scan.
    pub unfinished: Vec<PathBuf>,
}

impl Scanned {
    /// The old scan's root.
    #[must_use]
    pub fn root(&self) -> HostPath {
        self.tree.roots().next().map_or_else(
            || HostPath::empty(self.tree.host()),
            |r| self.tree.host_path(r),
        )
    }

    /// The old root's device, inode and mtime as it was read.
    #[must_use]
    pub fn root_stat(&self) -> Option<(u64, u64, i64)> {
        let r = self.tree.roots().next()?;
        let (dev, ino) = self.tree.dev_ino(r);
        Some((dev, ino, self.tree.mtime(r)))
    }

    /// Notes that what it holds was last read in full at `secs`, if that is earlier.
    pub fn read_in_full_at(&mut self, secs: u64) {
        self.full_scan = self.full_scan.min(secs);
    }
}

/// The root of `m`, a scan of this machine, when what it read can be kept by a scan of
/// `above`, a folder above that root: the scan ended complete or is still running and has
/// read the root's stat (which comes with its first entries), nothing is being deleted or
/// waits to be (staged, or to be put back), and no native command runs. `None` otherwise.
#[must_use]
pub fn keepable(m: &Model, above: &HostPath) -> Option<Ix> {
    let s = &m.status;
    let quiet = !s.jobs.is_empty()
        && !s.incomplete
        && s.fatal.is_none()
        && s.disconnected.is_none()
        && s.deleting.is_none()
        && s.staged.items == 0
        && s.undo.is_none()
        && s.undoing.is_none()
        && s.native.as_ref().is_none_or(|n| n.done.is_some());
    if !quiet {
        return None;
    }
    let root = m.tree.roots().next()?;
    let state = m.tree.dir(root).map(|d| d.state);
    let going = match state {
        Some(DirState::Complete) => true,
        // Its inode is 0 until its stat is in.
        Some(DirState::Pending) => m.tree.dev_ino(root).1 != 0,
        _ => false,
    };
    let below = m
        .tree
        .host_path(root)
        .rel(above)
        .is_some_and(|r| !r.is_empty());
    (going && below).then_some(root)
}

/// Copies the tree below the first visible root of `m`, a scan of this machine started at
/// `since`, when it can be kept by a scan of `above` ([`keepable`]), with the directories it
/// has not read to the end yet. `None` otherwise.
#[must_use]
pub fn scanned(m: &Model, above: &HostPath, since: u64) -> Option<Scanned> {
    let root = keepable(m, above)?;
    Some(Scanned {
        tree: loaded_copy(&m.tree, root),
        since,
        full_scan: since,
        journal: None,
        scan_ms: 0,
        unfinished: unfinished(&m.tree, root),
    })
}

/// The directories at or below `root` of `t` not read to the end yet, relative to `root`. A
/// directory is read to the end only once everything below it is, so only those not are
/// looked into.
fn unfinished(t: &Tree, root: Ix) -> Vec<PathBuf> {
    let pending = |ix| t.dir(ix).is_some_and(|d| d.state == DirState::Pending);
    let mut out = Vec::new();
    let mut stack = vec![(root, PathBuf::new())];
    while let Some((ix, rel)) = stack.pop() {
        if !pending(ix) {
            continue;
        }
        for c in t.children(ix).filter(|&c| pending(c)) {
            stack.push((c, rel.join(&*dfdu_tree::name_os(t.name(c)))));
        }
        out.push(rel);
    }
    out
}

/// The flags a loaded directory keeps, as a snapshot keeps them.
const KEPT_FLAGS: [NodeFlags; 4] = [
    NodeFlags::MOUNT_ROOT,
    NodeFlags::KERNEL,
    NodeFlags::NETWORK,
    NodeFlags::DUP_MOUNT,
];

/// What stat'ing node `ix` of `t` gave, as the scan read it.
fn meta_of(t: &Tree, ix: Ix) -> Meta {
    let (apparent, alloc) = t.raw_size(ix);
    let (dev, ino) = t.dev_ino(ix);
    Meta {
        kind: t.kind(ix),
        apparent,
        alloc,
        mtime: t.mtime(ix),
        dev,
        ino,
        nlink: if t.flags(ix).has(NodeFlags::HARDLINK) {
            2
        } else {
            1
        },
        mount: 0,
    }
}

/// The tree below `root` of `from` as a snapshot of it loads: a visible root named by its
/// path, without the flags it has as a child of its parent, which a graft decides again.
pub fn loaded_copy(from: &Tree, root: Ix) -> Tree {
    let mut t = Tree::new();
    t.set_host(from.host());
    let r = t.new_root(from.host_path(root).as_bytes(), &meta_of(from, root), true);
    if let Some(e) = from.error(root) {
        t.set_error(r, e);
    }
    copy_below(from, root, &mut t, r);
    t
}

/// Copies the entries below directory `from_dir` of `from` under directory `to` of `t`, and
/// closes every directory copied as a snapshot does, `to` among them, with its state.
fn copy_below(from: &Tree, from_dir: Ix, t: &mut Tree, to: Ix) {
    let mut links = HashSet::new();
    let mut stack = vec![(from_dir, to)];
    let mut dirs = vec![(from_dir, to)];
    while let Some((f, into)) = stack.pop() {
        for c in from.children(f) {
            let m = meta_of(from, c);
            if from.dir(c).is_some() {
                let flags = KEPT_FLAGS
                    .iter()
                    .filter(|&&k| from.flags(c).has(k))
                    .fold(NodeFlags::default(), |a, &k| a | k);
                let ix = t.add_dir_flagged(into, from.name(c), &m, flags);
                let own = t.own(ix);
                t.propagate_add(into, &own);
                if let Some(e) = from.error(c) {
                    t.set_error(ix, e);
                }
                stack.push((c, ix));
                dirs.push((c, ix));
            } else {
                let charged = !m.is_hardlink() || links.insert((m.dev, m.ino));
                let ix = t.add_file(into, from.name(c), &m, charged, from.error(c));
                let own = t.own(ix);
                t.propagate_add(into, &own);
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
        close(t, ix, state);
    }
}

/// Closes loaded directory `ix` with `state`, as a snapshot's are.
fn close(t: &mut Tree, ix: Ix, state: DirState) {
    if let Some(d) = t.dir_mut(ix) {
        d.state = state;
        d.outcome = state;
        d.pending = 0;
        d.listing_closed = true;
        d.reported = true;
    }
}

/// The tree a scan of `path` starts from to keep `scanned`, and the folders to list again
/// besides `path` (relative to it): `path` as its root with `stats[0]`, then each folder from
/// it down to the old root's parent with the stats after, then the old root with what was
/// read below it. The folders listed again are those between and the old scan's unfinished
/// ones. The folder and those between are left `Cancelled` (shown incomplete) until
/// the resume lists them. `None` when the old root is not below `path`, the stats are not one
/// for each folder down to the old root's parent, or one is not a directory.
#[must_use]
pub fn graft(scanned: Scanned, path: &HostPath, stats: &[Meta]) -> Option<(Tree, Vec<PathBuf>)> {
    let old = scanned.root();
    let rel: Vec<Vec<u8>> = old.rel(path)?.into_iter().map(<[u8]>::to_vec).collect();
    let (name, between) = rel.split_last()?;
    if stats.len() != between.len() + 1 || stats.iter().any(|m| m.kind != Kind::Dir) {
        return None;
    }
    let from = &scanned.tree;
    let from_root = from.roots().next()?;
    let mut t = Tree::new();
    t.set_host(from.host());
    let mut at = t.new_root(path.as_bytes(), &stats[0], true);
    let mut placeholders = vec![at];
    let mut changed = Vec::new();
    let mut rel_path = PathBuf::new();
    for (n, m) in between.iter().zip(&stats[1..]) {
        let ix = t.add_dir(at, n, m);
        let own = t.own(ix);
        t.propagate_add(at, &own);
        rel_path.push(&*dfdu_tree::name_os(n));
        changed.push(rel_path.clone());
        placeholders.push(ix);
        at = ix;
    }
    let ix = t.add_dir(at, name, &meta_of(from, from_root));
    let own = t.own(ix);
    t.propagate_add(at, &own);
    if let Some(e) = from.error(from_root) {
        t.set_error(ix, e);
    }
    copy_below(from, from_root, &mut t, ix);
    for p in placeholders {
        close(&mut t, p, DirState::Cancelled);
    }
    rel_path.push(&*dfdu_tree::name_os(name));
    changed.extend(scanned.unfinished.iter().map(|u| {
        if u.as_os_str().is_empty() {
            rel_path.clone()
        } else {
            rel_path.join(u)
        }
    }));
    t.bump_version();
    Some((t, changed))
}

/// What a scan of `path` keeping `scanned` starts from ([`graft`]), with the time the copy's
/// mtimes are judged against ([`Scanned::since`]) and the old root's device and inode when it
/// is a mount the new scan enters ([`stats_down`]), when `stat` (a job root's stat on this
/// machine) finds the folders down to the old root as [`stats_down`] wants them. A mount
/// entered is listed again, so that what is below it is kept. `None` otherwise: a mount on
/// the way, or an old root that became one, is the new scan's to judge, and a folder that
/// cannot be stat'ed its to report.
#[must_use]
pub fn above(
    scanned: Scanned,
    path: &HostPath,
    stat: impl Fn(&HostPath) -> Option<Meta>,
) -> Option<Above> {
    let (dev, ino, _) = scanned.root_stat()?;
    let old = scanned.root();
    let Down { stats, mount } = stats_down(&old, (dev, ino), path, stat)?;
    let since = scanned.since;
    let (tree, mut changed) = graft(scanned, path, &stats)?;
    if mount.is_some() {
        let rel: PathBuf = (old.rel(path)?.iter())
            .map(|n| dfdu_tree::name_os(n).into_owned())
            .collect();
        if !changed.contains(&rel) {
            changed.push(rel);
        }
    }
    Some(Above {
        tree,
        changed,
        since,
        mount,
    })
}

/// What a scan of a folder above an old scan's root starts from ([`above`]).
pub struct Above {
    /// The folder with the old root's tree grafted below it ([`graft`]).
    pub tree: Tree,
    /// The folders to list again, relative to the folder.
    pub changed: Vec<PathBuf>,
    /// When the old scan started ([`Scanned::since`]).
    pub since: u64,
    /// The old root's device and inode when it is a mount the new scan enters.
    pub mount: Option<(u64, u64)>,
}

/// The stats on the way down from a folder to an old scan's root ([`stats_down`]).
pub struct Down {
    /// The folder's and each folder's below it down to the old root's parent.
    pub stats: Vec<Meta>,
    /// The old root's device and inode when it is a mount of its own.
    pub mount: Option<(u64, u64)>,
}

/// The stats of `path` and each folder below it down to the old root's parent, and the old
/// root's device and inode when it is a mount of `path`'s, when `stat` finds `path` and every
/// folder down to `old` a directory on the device and mount of `path`, and `old` itself the
/// directory read as the old root (`old_at`, its device and inode), on that device and mount
/// too, or a mount on another device. `None` otherwise: a mount between them, or an old root
/// that is not the one read (it became a mount since, or was replaced).
#[must_use]
pub fn stats_down(
    old: &HostPath,
    old_at: (u64, u64),
    path: &HostPath,
    stat: impl Fn(&HostPath) -> Option<Meta>,
) -> Option<Down> {
    let rel = old.rel(path)?;
    let top = stat(path)?;
    let same = |m: &Meta| {
        m.kind == Kind::Dir
            && m.dev == top.dev
            && (m.mount == 0 || top.mount == 0 || m.mount == top.mount)
    };
    let mut stats = vec![top];
    let mut at = path.clone();
    for (i, name) in rel.iter().enumerate() {
        at.push(name);
        let m = stat(&at)?;
        if i + 1 < rel.len() {
            stats.push(Some(m).filter(same)?);
            continue;
        }
        // The old root.
        if same(&m) {
            return (old_at.0 == top.dev).then_some(Down { stats, mount: None });
        }
        let mount = m.kind == Kind::Dir && m.dev != top.dev && (m.dev, m.ino) == old_at;
        return mount.then_some(Down {
            stats,
            mount: Some(old_at),
        });
    }
    None
}

#[cfg(test)]
mod tests;
