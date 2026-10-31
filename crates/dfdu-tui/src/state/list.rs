//! The Files list: the order of the current directory's entries, the rows it shows, the
//! summary row of the entries new since the last sort, and where the list is scrolled to. The
//! cursor of each directory left is kept to restore it on the way back.

use std::cell::Cell;
use std::collections::HashMap;

use dfdu_tree::HostPath;

use crate::app::Selection;
use crate::state::order::Order;

#[derive(Default)]
pub struct List {
    pub order: Order,
    /// Indices into `snap.entries`, in display order.
    pub rows: Vec<usize>,
    /// Entries that arrived since the last sort, shown as one summary row.
    pub new_count: usize,
    pub new_largest: u64,
    /// Largest size among `rows`, for the bars.
    pub rows_largest: u64,
    pub scroll: usize,
    /// Counts the re-sorts of the rows; the treemap lays out again when it changes.
    pub sort_gen: u64,
    /// The user moved the cursor in the current directory; re-sorts then follow the selected
    /// row instead of returning to the top.
    pub(crate) moved: bool,
    pub(crate) cursors: HashMap<HostPath, Cursor>,
    /// Row where [`App::selected_index`](crate::app::App::selected_index) last found the
    /// selection.
    pub(crate) sel_hint: Cell<usize>,
}

/// Remembered position in a directory, restored when the user comes back to it.
#[derive(Clone, Debug)]
pub(crate) struct Cursor {
    pub(crate) selected: Option<Selection>,
    pub(crate) scroll: usize,
}
