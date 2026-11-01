//! Folding a new [`Snapshot`] into [`App`]: the row order, then the selection and scroll
//! position put back by the row-stability rules.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use dfdu_tree::NodeRef;

use dfdu_session::state::shown;

use super::{App, Selection};
use crate::model::snapshot::{EntryData, Snapshot};
use crate::state::order::{Order, Siblings};

/// One line describing a finished delete plan. On btrfs the freed space shows up in the free
/// space only after the transaction commits, and for snapshots after the cleaner ran.
fn delete_summary(d: &dfdu_engine::DeleteSummary) -> String {
    let done = match d.mode {
        Some(dfdu_engine::Mode::Trash) => "moved to the Trash",
        Some(dfdu_engine::Mode::Stage) => "staged",
        _ => "deleted",
    };
    let mut parts = vec![format!(
        "{done} {} ({})",
        d.deleted,
        crate::format::human(d.bytes)
    )];
    for (n, what) in [
        (d.stale, "changed since the scan"),
        (d.refused, "refused"),
        (d.failed, "failed"),
        (d.aborted, "stopped"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let mut s = parts.join(", ");
    if d.deleted > 0 && d.mode.is_some_and(dfdu_engine::Mode::undoable) {
        s.push_str("; U puts it back");
    }
    if d.btrfs {
        s.push_str("; btrfs frees the space when it commits (snapshots: in the background)");
    }
    s
}

#[cfg(test)]
mod tests;

/// How a snapshot just folded in differs from the one before.
#[derive(Clone, Copy, Default)]
struct Folded {
    /// Its entries are not the previous ones with new ones appended (another directory, or a
    /// rescan replaced some), so indices into the previous entries mean nothing in the new ones.
    replaced: bool,
    /// It is of another directory.
    dir_changed: bool,
    /// The scan finished with it.
    finished: bool,
    /// Its version differs from the previous one's: sizes or entries may have changed.
    changed: bool,
}

/// How [`App::update_order`] brings the rows up to date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reorder {
    /// An exact sort: another directory, a sort or filter the user asked for.
    Exact,
    /// The automatic sort, after the user has been idle.
    Auto,
    /// No sort: the new entries of a fresh snapshot are synced into the order.
    Sync,
    /// The new entries are synced and the rows on the first screen sorted, while the cursor is
    /// at the top ([`App::keeps_top_sorted`]). In tree mode this is [`Reorder::Sync`]: a row
    /// moving up would carry its expanded children along.
    Top,
}

impl Reorder {
    /// Whether the rows are sorted again. Sorting the first screen leaves the rows below it
    /// waiting for the automatic sort.
    fn resorts(self) -> bool {
        !matches!(self, Reorder::Sync | Reorder::Top)
    }
}

impl App {
    /// Whether a fresh snapshot sorts the rows on the first screen: while the user has not moved
    /// the cursor in this directory, or it is on the first row (`index`, before the snapshot).
    /// Deleted rows kept as placeholders hold still until the next re-sort.
    fn keeps_top_sorted(&self, index: Option<usize>) -> bool {
        (!self.session.list.moved || index == Some(0)) && !self.motion.has_ghosts()
    }

    pub fn refresh(&mut self, mut snap: Snapshot, now: Instant) {
        if let (Some(tree), Some(found)) = (&mut self.session.tree, &snap.tree) {
            tree.apply(found);
        }
        self.motion.carry_ghosts(&self.session.snap, &mut snap, now);
        if let Some(d) = &snap.last_delete
            && self
                .session
                .snap
                .last_delete
                .as_ref()
                .is_none_or(|p| p.plan != d.plan)
        {
            self.session.status = Some(delete_summary(d));
        }
        let dir_changed = snap.dir_path != self.session.snap.dir_path;
        // While a scan runs, entries are only added: the old entries are a prefix of the new
        // ones and indices into them stay valid, so nothing needs to be looked up by node.
        let appended = !dir_changed
            && snap.entries.len() >= self.session.snap.entries.len()
            && self
                .session
                .snap
                .entries
                .iter()
                .zip(&snap.entries)
                .all(|(a, b)| a.node == b.node);
        let swapped = if dir_changed || appended {
            HashMap::new()
        } else {
            self.carry_over_swapped(&snap)
        };
        let changed = snap.version != self.session.snap.version;
        if changed {
            self.session.sort_schedule.note_changed();
        }
        // A scan that just finished gets one more automatic sort right away (if the user is
        // idle), so the final order does not wait for the minimum interval.
        let finished = self.session.snap.busy && !snap.busy;
        if self.session.done_at.is_none() && !snap.busy && snap.dir_node.is_some() {
            self.session.done_at = Some(now);
        }
        if self.session.done_at.is_some() && snap.busy && !self.session.snap.busy {
            // A rescan started: time it from now.
            self.session.started = now;
            self.session.done_at = None;
        }
        let old_index = self.session.selected_index();
        let old = std::mem::replace(&mut self.session.snap, snap);
        if !dir_changed && !appended {
            let pos: HashMap<NodeRef, usize> = self
                .session
                .snap
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node, i))
                .collect();
            self.session.list.order.remap(|i| {
                let node = old.entries[i].node;
                pos.get(swapped.get(&node).unwrap_or(&node)).copied()
            });
        }
        drop(old);
        let folded = Folded {
            replaced: !appended,
            dir_changed,
            finished,
            changed,
        };
        self.update_order(Some(folded), old_index, now);
    }

    /// Applies sort requests and the automatic sort to the snapshot already held, for a frame
    /// drawn without reading the engine again.
    pub fn reorder(&mut self, now: Instant) {
        let old_index = self.session.selected_index();
        self.update_order(None, old_index, now);
    }

    /// Sorts or updates the row order, then puts the selection and scroll position back.
    /// `folded`: how the snapshot just folded in differs, `None` when none was.
    fn update_order(&mut self, folded: Option<Folded>, old_index: Option<usize>, now: Instant) {
        let fresh = folded.is_some();
        let Folded {
            replaced,
            dir_changed,
            finished,
            changed,
        } = folded.unwrap_or_default();
        let (requested, refilter) = self.session.sort_schedule.take_requests();
        let mode = if dir_changed || requested || refilter {
            Reorder::Exact
        } else if self
            .session
            .sort_schedule
            .auto_due(self.session.sort_pending(), finished, now)
        {
            Reorder::Auto
        } else if fresh && changed && self.keeps_top_sorted(old_index) {
            Reorder::Top
        } else if fresh {
            Reorder::Sync
        } else {
            // The entries and the order are those of the last frame: nothing below changes.
            return;
        };
        self.session.filter.prepare(&self.session.snap, replaced);
        let entries = &self.session.snap.entries;
        let (hide, filter) = (self.session.hide_hidden, &self.session.filter);
        let tree = self.tree_shown().is_some();
        // Rows below the top are left out of a list until the read without them comes in.
        let visible = |i: usize, e: &EntryData| {
            shown(hide, e) && (tree || e.parent == u32::MAX) && filter.matches(i)
        };
        // Only the filter changed: its rows are picked from the kept exact sort of every entry
        // it can show, in the order an exact sort of only those gives.
        let refiltered = mode == Reorder::Exact
            && !tree
            && refilter
            && !requested
            && !fresh
            && self
                .session
                .list
                .order
                .refilter(entries, visible, self.session.sort);
        match mode {
            _ if tree => {
                let how = match mode {
                    Reorder::Exact => Siblings::Exact,
                    Reorder::Auto => Siblings::Auto,
                    Reorder::Sync | Reorder::Top => Siblings::Keep,
                };
                // Between sorts a deleted row keeps its place, as the list keeps its row.
                let keep = how == Siblings::Keep;
                let visible = |i: usize, e: &EntryData| visible(i, e) || (keep && e.ghost);
                self.session
                    .list
                    .order
                    .sort_tree(entries, visible, self.session.sort, how);
            }
            Reorder::Exact if refiltered => {}
            Reorder::Exact => {
                self.session.list.order = Order::default();
                if filter.active() {
                    self.session
                        .list
                        .order
                        .sort_exact(entries, visible, self.session.sort);
                } else {
                    self.session.list.order.sort_exact_unfiltered(
                        entries,
                        visible,
                        self.session.sort,
                    );
                }
            }
            Reorder::Auto => self
                .session
                .list
                .order
                .sort_auto(entries, visible, self.session.sort),
            Reorder::Sync => self
                .session
                .list
                .order
                .sync(entries, visible, self.session.sort),
            Reorder::Top => {
                self.session.list.order.sort_top(
                    entries,
                    visible,
                    self.session.sort,
                    self.list_height,
                );
            }
        }
        let resorted = mode.resorts();
        if resorted {
            self.session.list.sort_gen += 1;
            self.session.sort_schedule.record_sort(now);
        }

        // The rows before a re-sort go to the motion hook; otherwise the buffer is reused.
        let old_rows = if resorted && !dir_changed {
            std::mem::take(&mut self.session.list.rows)
        } else {
            self.session.list.rows.clear();
            Vec::new()
        };
        self.session
            .list
            .rows
            .extend_from_slice(self.session.list.order.rows());
        if resorted && !dir_changed {
            let auto = mode == Reorder::Auto;
            self.motion
                .resorted(&old_rows, &self.session.list.rows, entries, auto);
        }
        self.session.list.tally(entries);
        self.restore_cursor(mode, dir_changed, old_index);
    }

    /// Finds the selection again in the rows just updated by `mode` and puts the scroll position
    /// back: a cursor the user has not moved stays at the top, a moved one on its screen line
    /// across a re-sort in the same directory.
    fn restore_cursor(&mut self, mode: Reorder, dir_changed: bool, old_index: Option<usize>) {
        self.fix_selection(old_index);
        if mode == Reorder::Top && !self.session.list.moved {
            self.session.select(0);
            self.scroll = 0;
        }
        if mode.resorts() && !dir_changed {
            if self.session.list.moved {
                // Keep the selected row on the same screen line.
                if let (Some(old), Some(new)) = (old_index, self.session.selected_index()) {
                    let line = old.saturating_sub(self.scroll);
                    self.scroll = new.saturating_sub(line);
                }
            } else {
                // The user has not moved the cursor here: stay at the top.
                self.session.select(0);
                self.scroll = 0;
            }
        }
    }

    /// A rescan replaces nodes. Rows whose node disappeared take over the new node with the same
    /// name and identity, so they keep their position instead of turning into "new" entries.
    /// Returns the replaced nodes, old to new.
    fn carry_over_swapped(&mut self, snap: &Snapshot) -> HashMap<NodeRef, NodeRef> {
        let mut swapped = HashMap::new();
        let present: HashSet<NodeRef> = snap.entries.iter().map(|e| e.node).collect();
        let before: HashSet<NodeRef> = self.session.snap.entries.iter().map(|e| e.node).collect();
        let fresh: HashMap<(&[u8], (u64, u64)), NodeRef> = snap
            .entries
            .iter()
            .filter(|e| !before.contains(&e.node))
            .map(|e| ((&e.name[..], e.dev_ino), e.node))
            .collect();
        if fresh.is_empty() {
            return swapped;
        }
        for old in self
            .session
            .snap
            .entries
            .iter()
            .filter(|e| !present.contains(&e.node))
        {
            if let Some(&new) = fresh.get(&(&old.name[..], old.dev_ino)) {
                swapped.insert(old.node, new);
                if let Some(sel) = self
                    .session
                    .selected
                    .as_mut()
                    .filter(|s| s.node == Some(old.node))
                {
                    sel.node = Some(new);
                }
            }
        }
        swapped
    }

    /// Re-finds the selection after a refresh: by reference, then by name and identity (rescan
    /// swap), then by the old position.
    fn fix_selection(&mut self, old_index: Option<usize>) {
        if self.session.selected_index().is_some() {
            return;
        }
        if let Some(sel) = &self.session.selected {
            let found = self.session.list.rows.iter().find(|&&i| {
                let e = &self.session.snap.entries[i];
                e.name[..] == sel.name[..] && (sel.dev_ino == (0, 0) || e.dev_ino == sel.dev_ino)
            });
            if let Some(&i) = found {
                self.session.selected = Some(Selection::of(&self.session.snap.entries[i]));
                return;
            }
        }
        if self.session.list.rows.is_empty() {
            self.session.selected = None;
            return;
        }
        let i = old_index.unwrap_or(0).min(self.session.list.rows.len() - 1);
        self.session.selected = Some(Selection::of(
            &self.session.snap.entries[self.session.list.rows[i]],
        ));
    }
}
