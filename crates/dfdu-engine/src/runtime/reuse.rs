//! Keeping what another scan of this machine read below a folder this engine scans
//! ([`crate::reuse`]): copying it out of the old engine, and starting the new scan from it.

use std::time::{SystemTime, UNIX_EPOCH};

use dfdu_scan::{Backend, JobOptions};
use dfdu_tree::HostPath;

use super::Engine;
use crate::applier::Command;
use crate::reuse::{Scanned, above, scanned};

/// Seconds since the Unix epoch now.
pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl<B: Backend> Engine<B> {
    /// What this scan read below its root, for a scan of `above`, a folder above it, to keep
    /// ([`Engine::scan_above`]): `None` unless this is a scan of this machine that ended
    /// complete, nothing is being deleted or waits to be, and `above` is above its root. The
    /// copy is taken under the read lock, in one go.
    pub fn scanned(&self, above: &HostPath) -> Option<Scanned> {
        if self.link.is_some() {
            return None;
        }
        self.read(|m| scanned(m, above, self.started_secs))
    }

    /// Scans `path` with `opts`, keeping what `scanned` read below it where it is unchanged:
    /// `path` and the folders down to the old root are listed again, and each directory below
    /// is kept when its stat is the one it was read with. Without `scanned`, or when `path`,
    /// a folder between or the old root is not a directory on the device and mount of `path`
    /// now (the old root became a mount, or one is in between), `path` is scanned as
    /// [`Command::Scan`] does. Stats those folders on the caller's thread. Whether it kept.
    pub fn scan_above(&self, path: &HostPath, scanned: Option<Scanned>, opts: JobOptions) -> bool {
        let start = scanned.filter(|_| self.link.is_none()).and_then(|s| {
            let backend = &self.shared.backend;
            above(s, path, |p| {
                backend
                    .stat_root(&backend.loc_from_path(&p.to_local()))
                    .ok()
            })
        });
        let Some((tree, changed, since)) = start else {
            self.send(Command::Scan {
                path: path.clone(),
                opts,
            });
            return false;
        };
        let Some(root) = tree.roots().next() else {
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
