//! The Files list: the order of the current directory's entries, the rows it shows and the
//! summary row of the entries new since the last sort. The selection of each directory left is
//! kept to restore it on the way back; where the list is scrolled to is the front end's.

use std::cell::Cell;
use std::collections::HashMap;

use dfdu_tree::HostPath;

use super::Selection;
use crate::order::Order;
use crate::snapshot::EntryData;

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
    /// Counts the re-sorts of the rows; the treemap lays out again when it changes.
    pub sort_gen: u64,
    /// The user moved the cursor in the current directory; re-sorts then follow the selected
    /// row instead of returning to the top.
    pub moved: bool,
    /// The selection of each directory left.
    pub(super) cursors: HashMap<HostPath, Option<Selection>>,
    /// Row where [`SessionState::selected_index`](super::SessionState::selected_index) last
    /// found the selection.
    pub(super) sel_hint: Cell<usize>,
}

impl List {
    /// Counts the entries new since the last sort and finds the largest of them and of the
    /// rows, once the order has been brought up to date.
    pub fn tally(&mut self, entries: &[EntryData]) {
        let largest = |rows: &[usize]| rows.iter().map(|&i| entries[i].size).max().unwrap_or(0);
        self.new_count = self.order.new_entries().len();
        self.new_largest = largest(self.order.new_entries());
        self.rows_largest = largest(&self.rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::SortSpec;
    use crate::snapshot::test_entry;

    #[test]
    fn tally_sizes_up_the_rows_and_the_new_entries() {
        let mut list = List::default();
        list.tally(&[]);
        assert_eq!(
            (list.new_count, list.new_largest, list.rows_largest),
            (0, 0, 0)
        );

        let all = |_: usize, _: &EntryData| true;
        let mut entries = vec![test_entry(0, "a", 40), test_entry(1, "b", 70)];
        list.order.sort_exact(&entries, all, SortSpec::default());
        // Two entries arrive between sorts: they are new, not rows.
        entries.push(test_entry(2, "c", 500));
        entries.push(test_entry(3, "d", 9));
        list.order.sync(&entries, all, SortSpec::default());
        list.rows = list.order.rows().to_vec();
        list.tally(&entries);
        assert_eq!(list.new_count, 2);
        assert_eq!(list.new_largest, 500);
        assert_eq!(list.rows_largest, 70);
    }
}
