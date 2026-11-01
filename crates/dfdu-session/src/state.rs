//! What a browser of a scan holds besides how it is drawn: the view shown, the directory opened
//! and its snapshot, the rows, the selection and the cursor of each directory left, the sort and
//! size mode, tree mode, the marks, the filters, the status line, and what the scan history and
//! the lookups outside the scan found. A front end keeps its own view state (scroll, overlays,
//! layout) next to a [`SessionState`].

mod cursor;
mod list;
mod schedule;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dfdu_engine::SizeMode;
use dfdu_tree::Target;
use dfdu_tree::filetype::TypeFilter;

use crate::cleanup::Cleanup;
use crate::extras::Extras;
use crate::filter::Filter;
use crate::marks::Marks;
use crate::order::SortSpec;
use crate::outside::Outside;
use crate::snapshot::{EntryData, Snapshot};
use crate::tree::TreeState;
use crate::treemap::MapState;
use crate::view::View;
pub use cursor::{Moved, Selection};
pub use list::List;
pub use schedule::{IDLE_BEFORE_SORT, MIN_SORT_INTERVAL, SortSchedule};

/// The status line after a key that deletes, or marks, in a read-only browser.
pub const READ_ONLY: &str = "read-only: nothing is deleted";

/// Why a directory cannot be marked or deleted while the file type filter is on.
pub const NO_FOLDERS: &str = "folders cannot be marked or deleted while the type filter is on: their sizes count only some files (Esc shows all)";

/// What the header says about the cached scan shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cached {
    /// It is being read.
    Loading,
    /// It is shown, written at `written` (seconds since the Unix epoch), and being brought up
    /// to date.
    Refreshing { written: u64 },
}

pub struct SessionState {
    /// The view shown; Cleanup without rules shows as Files ([`SessionState::current_view`]).
    pub view: View,
    pub target: Target,
    pub snap: Snapshot,
    /// The Files list's order and rows, and the cursor of each directory left.
    pub list: List,
    pub sort: SortSpec,
    pub size_mode: SizeMode,
    /// Tree mode: the expanded directories and the fold threshold. `None` when off.
    pub tree: Option<TreeState>,
    pub hide_hidden: bool,
    pub selected: Option<Selection>,
    pub status: Option<String>,
    /// Estimates asked for so far; the latest one's id.
    pub estimates: u64,
    pub started: Instant,
    pub done_at: Option<Instant>,
    pub marks: Marks,
    pub filter: Filter,
    /// The file type filter; empty when off.
    pub types: TypeFilter,
    /// The user's home directory, shown as `~` in the breadcrumbs; `None` for a scan that is
    /// not of this machine.
    pub home: Option<PathBuf>,
    /// The sizes of the last recorded scan of this root, to show what grew since.
    pub growth: Option<Arc<dfdu_store::history::Growth>>,
    /// The scan shown is the cached one, being read or brought up to date.
    pub cached: Option<Cached>,
    /// Mounts the scan did not enter and links out of it, as looked up (`outside`).
    pub outside: Outside,
    /// Nothing is deleted: the actions that delete are left out and cleanup commands run only
    /// as dry runs.
    pub read_only: bool,
    /// When the rows are sorted again.
    pub sort_schedule: SortSchedule,
    /// The Map view's depth and colouring.
    pub map: MapState,
    /// The cleanup view's cursor, open rows, filter and what it marked.
    pub cleanup: Cleanup,
    /// What the features read from the engine model with the last read.
    pub extras: Extras,
}

/// Whether `e` gets a row when no filter hides it.
pub fn shown(hide_hidden: bool, e: &EntryData) -> bool {
    !e.ghost && (!hide_hidden || !e.hidden())
}

impl SessionState {
    /// An empty browser opened at `now`.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        SessionState {
            view: View::default(),
            target: Target::root(),
            snap: Snapshot::default(),
            list: List::default(),
            sort: SortSpec::default(),
            size_mode: SizeMode::Disk,
            tree: None,
            hide_hidden: false,
            selected: None,
            status: None,
            estimates: 0,
            started: now,
            done_at: None,
            marks: Marks::default(),
            filter: Filter::default(),
            types: TypeFilter::default(),
            home: None,
            growth: None,
            cached: None,
            outside: Outside::default(),
            read_only: false,
            sort_schedule: SortSchedule::new(now),
            map: MapState::default(),
            cleanup: Cleanup::default(),
            extras: Extras::default(),
        }
    }

    /// True when an automatic re-sort is waiting for the user to be idle.
    pub fn sort_pending(&self) -> bool {
        self.list.new_count > 0 || self.sort_schedule.changed()
    }

    /// When the front end should wake for an automatic re-sort, if one is pending.
    pub fn next_sort_at(&self) -> Option<Instant> {
        self.sort_pending().then(|| self.sort_schedule.due_at())
    }

    /// Asks for an exact sort before the next frame.
    pub fn request_sort(&mut self) {
        self.sort_schedule.request_sort();
    }

    /// Asks for the rows of a changed filter before the next frame: those an exact sort gives.
    pub fn request_refilter(&mut self) {
        self.sort_schedule.request_refilter();
    }

    /// Makes the order keep an exact sort of every entry the filter can show, unless it holds
    /// one, so the keystrokes that change the filter only pick their rows from it.
    pub fn keep_exact_order(&mut self) {
        let hide = self.hide_hidden;
        self.list
            .order
            .keep_exact(&self.snap.entries, |_, e| shown(hide, e), self.sort);
    }

    /// Records user input at `now`: automatic re-sorts wait until the user is idle.
    pub fn note_input(&mut self, now: Instant) {
        self.sort_schedule.note_input(now);
    }

    /// The file type filter is on: folders cannot be marked, as their sizes count only some
    /// files.
    pub fn types_active(&self) -> bool {
        !self.types.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod tests;
