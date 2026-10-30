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

pub(crate) use dfdu_engine::{describe, linked_note, rel_text, tilde};
use dfdu_rules::{Linked, RuleSet};
use dfdu_sys::MountInfo;
use dfdu_tree::HostPath;
use ratatui::text::Line;

use crate::app::App;
use crate::model::snapshot::{EntryData, Mark};
use crate::theme::human;

/// Most links out of the scan the cleanup view names.
const VIEW_MAX: usize = 3;

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

    /// Records `info` for the mount at `path`, as a lookup would.
    #[cfg(test)]
    pub fn set_mount(&mut self, path: HostPath, info: Option<MountInfo>) {
        self.mounts.insert(path, Arc::new(OnceLock::from(info)));
    }

    #[cfg(test)]
    pub fn set_linked(&mut self, linked: Vec<Linked>) {
        self.linked = Some(Arc::new(OnceLock::from(linked)));
    }
}

/// A mount the scan listed but did not enter.
fn left_out(e: &EntryData) -> bool {
    matches!(e.mark, Mark::OtherFs | Mark::Network)
}

/// Starts the lookups the snapshot held needs: the mounts among its entries not looked up yet,
/// and, once the rules and the root are known, the links out of the scan.
pub fn request(app: &mut App) {
    let o = &mut app.outside;
    if !o.local {
        return;
    }
    let mut new = Vec::new();
    for (i, _) in (app.snap.entries.iter().enumerate()).filter(|(_, e)| left_out(e)) {
        let path = crate::state::tree::path_of(&app.snap, i);
        if !o.mounts.contains_key(&path) {
            let lock = Arc::new(OnceLock::new());
            o.mounts.insert(path.clone(), lock.clone());
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
            o.mounts.retain(|_, l| l.get().is_some());
        }
    }
    if let (None, Some(rules)) = (&o.linked, &app.snap.rules)
        && !app.snap.root_path.is_empty()
    {
        let (rules, root) = (rules.clone(), app.snap.root_path.to_local());
        let lock: Arc<OnceLock<Vec<Linked>>> = Arc::default();
        let set = lock.clone();
        let _ = std::thread::Builder::new()
            .name("dfdu-links".into())
            .spawn(move || {
                let _ = set.set(dfdu_rules::linked_outside(&rules, &root));
            });
        o.linked = Some(lock);
    }
}

/// What the details pane says about the mount at `path`, which the scan did not enter: its type
/// with its used and total space, once looked up (only the type for a network filesystem,
/// which is never asked), and the key that scans it.
#[must_use]
pub fn pane_lines(app: &App, path: &HostPath) -> (Option<String>, String) {
    let info = app.outside.mount(path).map(|i| match (i.used, i.total) {
        (Some(u), Some(t)) => format!(
            "{} · {} used of {}, not counted here",
            i.fstype,
            human(u),
            human(t)
        ),
        (Some(u), None) => format!("{} · {} used, not counted here", i.fstype, human(u)),
        _ => i.fstype.clone(),
    });
    let key = crate::app::keys::first_key(crate::app::keys::Action::EnterMount);
    (info, format!("{key} scans it"))
}

/// The note after the name of the row of entry `index`, a mount that was not entered.
#[must_use]
pub fn row_note(app: &App, index: usize) -> String {
    let e = &app.snap.entries[index];
    let path = crate::state::tree::path_of(&app.snap, index);
    let what = match e.mark {
        Mark::Network => "network filesystem",
        _ => "other filesystem",
    };
    let key = crate::app::keys::first_key(crate::app::keys::Action::EnterMount);
    match app.outside.mount(&path) {
        // The used space is on the other filesystem, not in this directory's size.
        Some(info) if info.used.is_some() => {
            format!("  {what} ({}, not counted): {key} scans it", describe(info))
        }
        Some(info) => format!("  {what} ({}): {key} scans it", info.fstype),
        None => format!("  {what}: {key} scans it"),
    }
}

/// The cleanup view's lines about rule locations behind links out of the scan.
#[must_use]
pub fn view_lines(app: &App, rules: &RuleSet) -> Vec<Line<'static>> {
    let linked = app.outside.linked();
    let mut out: Vec<Line> = linked
        .iter()
        .take(VIEW_MAX)
        .map(|l| {
            let note = linked_note(rules, l, app.home.as_deref(), "dfdu");
            Line::styled(format!(" {note}"), app.theme.fg_dim)
        })
        .collect();
    if let Some(more) = dfdu_engine::more_links(linked.len(), VIEW_MAX) {
        out.push(Line::styled(format!(" {more}"), app.theme.fg_dim));
    }
    out
}

#[cfg(test)]
mod tests;
