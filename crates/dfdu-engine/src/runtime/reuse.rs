//! Keeping what another scan of this machine read below a folder this engine scans
//! ([`crate::reuse`]): copying it out of the old engine, and starting the new scan from it.

use std::time::{SystemTime, UNIX_EPOCH};

use dfdu_scan::{Backend, JobOptions};
use dfdu_tree::HostPath;

use super::Engine;
use crate::applier::Command;
use crate::reuse::{Scanned, above, keepable, scanned, stats_down};

/// Seconds since the Unix epoch now.
pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl<B: Backend> Engine<B> {
    /// What this scan read below its root, for a scan of `above`, a folder above it, to keep
    /// ([`Engine::scan_above`]): `None` unless this is a scan of this machine that ended
    /// complete or is still running, nothing is being deleted or waits to be, and `above` is
    /// above its root. The copy is taken under the read lock, in one go.
    pub fn scanned(&self, above: &HostPath) -> Option<Scanned> {
        if self.link.is_some() {
            return None;
        }
        self.read(|m| scanned(m, above, self.started_secs))
    }

    /// Whether a scan of `above` would keep what this scan read ([`Engine::scanned`] and
    /// [`Engine::scan_above`]) if it started now, without copying it. Stats the folders down
    /// to this scan's root on the caller's thread.
    pub fn keeps(&self, above: &HostPath) -> bool {
        if self.link.is_some() {
            return false;
        }
        let Some((old, dev)) =
            self.read(|m| keepable(m, above).map(|r| (m.tree.host_path(r), m.tree.dev_ino(r).0)))
        else {
            return false;
        };
        let backend = &self.shared.backend;
        stats_down(&old, dev, above, |p| {
            backend
                .stat_root(&backend.loc_from_path(&p.to_local()))
                .ok()
        })
        .is_some()
    }

    /// Scans `path` with `opts`, keeping what `scanned` read below it where it is unchanged:
    /// `path` and the folders down to the old root are listed again, and each directory below
    /// is kept when its stat is the one it was read with. Without `scanned`, or when `path`,
    /// a folder between or the old root is not a directory on the device and mount of `path`
    /// now (the old root became a mount, or one is in between), or the tree it would start
    /// from has no root, `path` is scanned as [`Command::Scan`] does. Stats those folders on the caller's thread. Whether it kept.
    pub fn scan_above(&self, path: &HostPath, scanned: Option<Scanned>, opts: JobOptions) -> bool {
        let start = scanned.filter(|_| self.link.is_none()).and_then(|s| {
            let backend = &self.shared.backend;
            above(s, path, |p| {
                backend
                    .stat_root(&backend.loc_from_path(&p.to_local()))
                    .ok()
            })
        });
        let Some((tree, changed, since, root)) = start.and_then(|(t, c, s)| {
            let r = t.roots().next()?;
            Some((t, c, s, r))
        }) else {
            self.send(Command::Scan {
                path: path.clone(),
                opts,
            });
            return false;
        };
        let node = tree.node_ref(root);
        self.load(tree);
        self.send(Command::Resume {
            node,
            opts,
            changed: Some(changed),
            since,
            root: None,
        });
        true
    }
}
