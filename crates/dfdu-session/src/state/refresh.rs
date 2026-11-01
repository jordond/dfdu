//! Folding a new [`Snapshot`] into the state: the row order, then the selection put back by the
//! row-stability rules ([`SessionState::refresh`]). What a front end does with a re-sort (move
//! the rows on screen, keep the selected row on its screen line) comes back as a
//! [`Reordered`].

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use dfdu_tree::NodeRef;

use super::{Selection, SessionState, shown};
use crate::format::human;
use crate::order::{Order, Siblings};
use crate::snapshot::{EntryData, Snapshot};

/// One line describing a finished delete plan. On btrfs the freed space shows up in the free
/// space only after the transaction commits, and for snapshots after the cleaner ran.
fn delete_summary(d: &dfdu_engine::DeleteSummary) -> String {
    let done = match d.mode {
        Some(dfdu_engine::Mode::Trash) => "moved to the Trash",
        Some(dfdu_engine::Mode::Stage) => "staged",
        _ => "deleted",
    };
    let mut parts = vec![format!("{done} {} ({})", d.deleted, human(d.bytes))];
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

/// How the rows are brought up to date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reorder {
    /// An exact sort: another directory, a sort or filter the user asked for.
    Exact,
    /// The automatic sort, after the user has been idle.
    Auto,
    /// No sort: the new entries of a fresh snapshot are synced into the order.
    Sync,
    /// The new entries are synced and the rows on the first screen sorted, while the cursor is
    /// at the top ([`SessionState::keeps_top_sorted`]). In tree mode this is
    /// [`Reorder::Sync`]: a row moving up would carry its expanded children along.
    Top,
}

impl Reorder {
    /// Whether the rows are sorted again. Sorting the first screen leaves the rows below it
    /// waiting for the automatic sort.
    fn resorts(self) -> bool {
        !matches!(self, Reorder::Sync | Reorder::Top)
    }
}

/// Where a front end scrolls its list after the rows were brought up to date.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scroll {
    /// Where it was.
    #[default]
    Keep,
    /// To the top: the user has not moved the cursor here.
    Top,
    /// By the selected row's move from row `from` to row `to` in a re-sort, so it stays on the
    /// same screen line ([`Scroll::apply`]).
    Follow { from: usize, to: usize },
}

impl Scroll {
    /// The first row on screen after this, from `scroll` before.
    #[must_use]
    pub fn apply(self, scroll: usize) -> usize {
        match self {
            Scroll::Keep => scroll,
            Scroll::Top => 0,
            Scroll::Follow { from, to } => to.saturating_sub(from.saturating_sub(scroll)),
        }
    }
}

/// What bringing the rows up to date leaves to the front end.
#[derive(Clone, Debug, Default)]
#[must_use]
pub struct Reordered {
    /// The rows before a re-sort in the same directory, for a front end that shows the rows
    /// moving to their new places; `None` without one.
    pub resorted: Option<Resorted>,
    pub scroll: Scroll,
}

/// The rows before a re-sort in the same directory.
#[derive(Clone, Debug)]
pub struct Resorted {
    pub before: Vec<usize>,
    /// It was the automatic sort, not one the user asked for.
    pub auto: bool,
}

impl SessionState {
    /// Whether a fresh snapshot sorts the rows on the first screen: while the user has not moved
    /// the cursor in this directory, or it is on the first row (`index`, before the snapshot).
    /// Rows a front end holds still (deleted rows kept as placeholders, `hold`) wait for the
    /// next re-sort.
    fn keeps_top_sorted(&self, index: Option<usize>, hold: bool) -> bool {
        (!self.list.moved || index == Some(0)) && !hold
    }

    /// Folds in `snap`, read at `now`, and brings the rows up to date. `hold`: the front end
    /// holds rows still on screen until the next re-sort, so the first screen is not sorted.
    pub fn refresh(&mut self, snap: Snapshot, now: Instant, hold: bool) -> Reordered {
        if let (Some(tree), Some(found)) = (&mut self.tree, &snap.tree) {
            tree.apply(found);
        }
        if let Some(d) = &snap.last_delete
            && (self.snap.last_delete.as_ref()).is_none_or(|p| p.plan != d.plan)
        {
            self.status = Some(delete_summary(d));
        }
        let dir_changed = snap.dir_path != self.snap.dir_path;
        // While a scan runs, entries are only added: the old entries are a prefix of the new
        // ones and indices into them stay valid, so nothing needs to be looked up by node.
        let appended = !dir_changed
            && snap.entries.len() >= self.snap.entries.len()
            && (self.snap.entries.iter())
                .zip(&snap.entries)
                .all(|(a, b)| a.node == b.node);
        let swapped = if dir_changed || appended {
            HashMap::new()
        } else {
            self.carry_over_swapped(&snap)
        };
        let changed = snap.version != self.snap.version;
        if changed {
            self.sort_schedule.note_changed();
        }
        // A scan that just finished gets one more automatic sort right away (if the user is
        // idle), so the final order does not wait for the minimum interval.
        let finished = self.snap.busy && !snap.busy;
        if self.done_at.is_none() && !snap.busy && snap.dir_node.is_some() {
            self.done_at = Some(now);
        }
        if self.done_at.is_some() && snap.busy && !self.snap.busy {
            // A rescan started: time it from now.
            self.started = now;
            self.done_at = None;
        }
        let old_index = self.selected_index();
        let old = std::mem::replace(&mut self.snap, snap);
        if !dir_changed && !appended {
            let pos: HashMap<NodeRef, usize> = (self.snap.entries.iter())
                .enumerate()
                .map(|(i, e)| (e.node, i))
                .collect();
            self.list.order.remap(|i| {
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
        self.update_order(Some(folded), old_index, now, hold)
    }

    /// Applies sort requests and the automatic sort to the snapshot already held, for a frame
    /// drawn without reading the engine again.
    pub fn reorder(&mut self, now: Instant, hold: bool) -> Reordered {
        let old_index = self.selected_index();
        self.update_order(None, old_index, now, hold)
    }

    /// Sorts or updates the row order, then puts the selection back. `folded`: how the
    /// snapshot just folded in differs, `None` when none was.
    fn update_order(
        &mut self,
        folded: Option<Folded>,
        old_index: Option<usize>,
        now: Instant,
        hold: bool,
    ) -> Reordered {
        let fresh = folded.is_some();
        let Folded {
            replaced,
            dir_changed,
            finished,
            changed,
        } = folded.unwrap_or_default();
        let (requested, refilter) = self.sort_schedule.take_requests();
        let mode = if dir_changed || requested || refilter {
            Reorder::Exact
        } else if (self.sort_schedule).auto_due(self.sort_pending(), finished, now) {
            Reorder::Auto
        } else if fresh && changed && self.keeps_top_sorted(old_index, hold) {
            Reorder::Top
        } else if fresh {
            Reorder::Sync
        } else {
            // The entries and the order are those of the last frame: nothing below changes.
            return Reordered::default();
        };
        self.filter.prepare(&self.snap, replaced);
        let entries = &self.snap.entries;
        let (hide, filter) = (self.hide_hidden, &self.filter);
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
            && self.list.order.refilter(entries, visible, self.sort);
        let order = &mut self.list.order;
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
                order.sort_tree(entries, visible, self.sort, how);
            }
            Reorder::Exact if refiltered => {}
            Reorder::Exact => {
                *order = Order::default();
                if filter.active() {
                    order.sort_exact(entries, visible, self.sort);
                } else {
                    order.sort_exact_unfiltered(entries, visible, self.sort);
                }
            }
            Reorder::Auto => order.sort_auto(entries, visible, self.sort),
            Reorder::Sync => order.sync(entries, visible, self.sort),
            Reorder::Top => order.sort_top(entries, visible, self.sort, self.viewport.rows),
        }
        let resorted = mode.resorts();
        if resorted {
            self.list.sort_gen += 1;
            self.sort_schedule.record_sort(now);
        }

        // The rows before a re-sort go back to the front end; otherwise the buffer is reused.
        let before = if resorted && !dir_changed {
            std::mem::take(&mut self.list.rows)
        } else {
            self.list.rows.clear();
            Vec::new()
        };
        self.list.rows.extend_from_slice(self.list.order.rows());
        self.list.tally(&self.snap.entries);
        let scroll = self.restore_cursor(mode, dir_changed, old_index);
        Reordered {
            resorted: (resorted && !dir_changed).then(|| Resorted {
                before,
                auto: mode == Reorder::Auto,
            }),
            scroll,
        }
    }

    /// Finds the selection again in the rows just updated by `mode`, and says where the front
    /// end's scroll goes: a cursor the user has not moved stays at the top, a moved one on its
    /// screen line across a re-sort in the same directory.
    fn restore_cursor(
        &mut self,
        mode: Reorder,
        dir_changed: bool,
        old_index: Option<usize>,
    ) -> Scroll {
        self.fix_selection(old_index);
        let mut scroll = Scroll::Keep;
        if mode == Reorder::Top && !self.list.moved {
            self.select(0);
            scroll = Scroll::Top;
        }
        if mode.resorts() && !dir_changed {
            if self.list.moved {
                // Keep the selected row on the same screen line.
                if let (Some(from), Some(to)) = (old_index, self.selected_index()) {
                    scroll = Scroll::Follow { from, to };
                }
            } else {
                // The user has not moved the cursor here: stay at the top.
                self.select(0);
                scroll = Scroll::Top;
            }
        }
        scroll
    }

    /// A rescan replaces nodes. Rows whose node disappeared take over the new node with the same
    /// name and identity, so they keep their position instead of turning into "new" entries.
    /// Returns the replaced nodes, old to new.
    fn carry_over_swapped(&mut self, snap: &Snapshot) -> HashMap<NodeRef, NodeRef> {
        let mut swapped = HashMap::new();
        let present: HashSet<NodeRef> = snap.entries.iter().map(|e| e.node).collect();
        let before: HashSet<NodeRef> = self.snap.entries.iter().map(|e| e.node).collect();
        let fresh: HashMap<(&[u8], (u64, u64)), NodeRef> = (snap.entries.iter())
            .filter(|e| !before.contains(&e.node))
            .map(|e| ((&e.name[..], e.dev_ino), e.node))
            .collect();
        if fresh.is_empty() {
            return swapped;
        }
        for old in (self.snap.entries.iter()).filter(|e| !present.contains(&e.node)) {
            if let Some(&new) = fresh.get(&(&old.name[..], old.dev_ino)) {
                swapped.insert(old.node, new);
                if let Some(sel) = (self.selected.as_mut()).filter(|s| s.node == Some(old.node)) {
                    sel.node = Some(new);
                }
            }
        }
        swapped
    }

    /// Re-finds the selection after a refresh: by reference, then by name and identity (rescan
    /// swap), then by the old position.
    fn fix_selection(&mut self, old_index: Option<usize>) {
        if self.selected_index().is_some() {
            return;
        }
        if let Some(sel) = &self.selected {
            let found = self.list.rows.iter().find(|&&i| {
                let e = &self.snap.entries[i];
                e.name[..] == sel.name[..] && (sel.dev_ino == (0, 0) || e.dev_ino == sel.dev_ino)
            });
            if let Some(&i) = found {
                self.selected = Some(Selection::of(&self.snap.entries[i]));
                return;
            }
        }
        if self.list.rows.is_empty() {
            self.selected = None;
            return;
        }
        let i = old_index.unwrap_or(0).min(self.list.rows.len() - 1);
        self.selected = Some(Selection::of(&self.snap.entries[self.list.rows[i]]));
    }
}

#[cfg(test)]
mod tests;
