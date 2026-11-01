//! What the scan did not enter: mounts it listed but stayed out of (`-x`), shown with their
//! filesystem type and used space, and rule locations behind a link that leads out of the scan
//! ([`dfdu_rules::linked_outside`]). The browser looks both up on this machine in the
//! background, never under the engine's lock; the wording both front ends share is in
//! dfdu-engine ([`dfdu_engine::describe`], [`dfdu_engine::linked_note`]).
//! A mount's used space is not part of any size in the tree: it is on another filesystem. A
//! network filesystem is never asked for it, since a dead one does not answer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use dfdu_engine::{describe, human};
use dfdu_rules::Linked;
use dfdu_sys::MountInfo;
use dfdu_tree::HostPath;

use crate::snapshot::{EntryData, Mark, Snapshot};

/// The browser's lookups, by mount point, and the links out of the scan.
#[derive(Debug, Default)]
pub struct Outside {
    /// Look things up on this machine; off for imported and remote scans, and in tests.
    pub local: bool,
    mounts: HashMap<HostPath, Arc<OnceLock<Option<MountInfo>>>>,
    linked: Option<Arc<OnceLock<Vec<Linked>>>>,
    /// Lookups that had ended when last polled.
    done: usize,
}

impl Outside {
    /// The mount at `path`, once looked up.
    #[must_use]
    pub fn mount(&self, path: &HostPath) -> Option<&MountInfo> {
        self.mounts.get(path)?.get()?.as_ref()
    }

    /// The links out of the scan that rule locations go through, once looked up.
    #[must_use]
    pub fn linked(&self) -> &[Linked] {
        self.linked
            .as_ref()
            .and_then(|l| l.get())
            .map_or(&[], |v| v)
    }

    /// Takes in what the lookups found: true when one ended since the last call.
    pub fn poll(&mut self) -> bool {
        let done = self.mounts.values().filter(|l| l.get().is_some()).count()
            + usize::from(self.linked.as_ref().is_some_and(|l| l.get().is_some()));
        let changed = done != self.done;
        self.done = done;
        changed
    }

    /// A lookup is still running.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.mounts.values().any(|l| l.get().is_none())
            || self.linked.as_ref().is_some_and(|l| l.get().is_none())
    }

    /// Holds the lookup of the links out of the scan, run on a thread when `started`. Without
    /// one nothing is looked up, so none are found.
    fn look_up_linked(&mut self, lock: Arc<OnceLock<Vec<Linked>>>, started: bool) {
        if !started {
            let _ = lock.set(Vec::new());
        }
        self.linked = Some(lock);
    }

    /// Starts the lookups `snap` needs: the mounts among its entries not looked up yet, and,
    /// once the rules and the root are known, the links out of the scan.
    pub fn request(&mut self, snap: &Snapshot) {
        if !self.local {
            return;
        }
        let mut new = Vec::new();
        for (i, _) in (snap.entries.iter().enumerate()).filter(|(_, e)| left_out(e)) {
            let path = crate::tree::path_of(snap, i);
            if !self.mounts.contains_key(&path) {
                let lock = Arc::new(OnceLock::new());
                self.mounts.insert(path.clone(), lock.clone());
                new.push((path, lock));
            }
        }
        if !new.is_empty() {
            let spawned = std::thread::Builder::new()
                .name("dfdu-mounts".into())
                .spawn(move || {
                    let paths: Vec<PathBuf> = new.iter().map(|(p, _)| p.to_local()).collect();
                    let found = dfdu_sys::mounts_at(&paths, dfdu_sys::DISK_TIMEOUT);
                    for ((_, lock), info) in new.iter().zip(found) {
                        let _ = lock.set(info);
                    }
                });
            if spawned.is_err() {
                // Without a thread nothing is looked up; the rows say what they always said.
                self.mounts.retain(|_, l| l.get().is_some());
            }
        }
        if let (None, Some(rules)) = (&self.linked, &snap.rules)
            && !snap.root_path.is_empty()
        {
            let (rules, root) = (rules.clone(), snap.root_path.to_local());
            let lock: Arc<OnceLock<Vec<Linked>>> = Arc::default();
            let set = lock.clone();
            let spawned = std::thread::Builder::new()
                .name("dfdu-links".into())
                .spawn(move || {
                    let _ = set.set(dfdu_rules::linked_outside(&rules, &root));
                });
            self.look_up_linked(lock, spawned.is_ok());
        }
    }

    /// What the details pane says about the mount at `path`, which the scan did not enter: its
    /// type with its used and total space, once looked up (only the type for a network
    /// filesystem, which is never asked).
    #[must_use]
    pub fn pane_info(&self, path: &HostPath) -> Option<String> {
        self.mount(path).map(|i| match (i.used, i.total) {
            (Some(u), Some(t)) => format!(
                "{} · {} used of {}, not counted here",
                i.fstype,
                human(u),
                human(t)
            ),
            (Some(u), None) => format!("{} · {} used, not counted here", i.fstype, human(u)),
            _ => i.fstype.clone(),
        })
    }

    /// The note after the name of the row of entry `index` of `snap`, a mount that was not
    /// entered, without the key that scans it: what it is, and how much it uses once looked up.
    #[must_use]
    pub fn row_note(&self, snap: &Snapshot, index: usize) -> String {
        let e = &snap.entries[index];
        let path = crate::tree::path_of(snap, index);
        let what = match e.mark {
            Mark::Network => "network filesystem",
            _ => "other filesystem",
        };
        match self.mount(&path) {
            // The used space is on the other filesystem, not in this directory's size.
            Some(info) if info.used.is_some() => {
                format!("{what} ({}, not counted)", describe(info))
            }
            Some(info) => format!("{what} ({})", info.fstype),
            None => what.to_owned(),
        }
    }

    /// Records `info` for the mount at `path`, as a lookup would.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_mount(&mut self, path: HostPath, info: Option<MountInfo>) {
        self.mounts.insert(path, Arc::new(OnceLock::from(info)));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_linked(&mut self, linked: Vec<Linked>) {
        self.linked = Some(Arc::new(OnceLock::from(linked)));
    }
}

/// A mount the scan listed but did not enter.
fn left_out(e: &EntryData) -> bool {
    matches!(e.mark, Mark::OtherFs | Mark::Network)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A links lookup whose thread could not be started finds none, so nothing is left pending.
    #[test]
    fn a_links_lookup_without_a_thread_is_not_pending() {
        let mut o = Outside::default();
        o.look_up_linked(Arc::default(), true);
        assert!(o.pending());
        o.look_up_linked(Arc::default(), false);
        assert!(!o.pending());
        assert_eq!(o.linked().len(), 0);
        assert!(o.poll());
    }
}
