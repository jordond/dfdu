//! The selection and the cursor moves: the selected row, found again after a rescan by name
//! and identity, the moves through the rows, and the way into and out of directories, which
//! remembers the cursor of each directory left to restore it on the way back.

use dfdu_tree::{HostPath, NodeRef};

use super::App;
use crate::model::snapshot::{EntryData, Target};

/// The selected entry: its reference, plus name and identity to find it again after a rescan.
#[derive(Clone, Debug)]
pub struct Selection {
    /// `None` when only the name is known (going up to the parent directory).
    pub node: Option<NodeRef>,
    pub name: Vec<u8>,
    pub dev_ino: (u64, u64),
}

impl Selection {
    pub(crate) fn of(e: &EntryData) -> Self {
        Selection {
            node: Some(e.node),
            name: e.name.to_vec(),
            dev_ino: e.dev_ino,
        }
    }

    /// The entry named `name` below the directory opened, to find again by name once its
    /// rows are read.
    fn named(name: Vec<u8>) -> Self {
        Selection {
            node: None,
            name,
            dev_ino: (0, 0),
        }
    }
}

/// Remembered position in a directory, restored when the user comes back to it.
#[derive(Clone, Debug)]
pub(super) struct Cursor {
    selected: Option<Selection>,
    scroll: usize,
}

impl App {
    pub fn selected_index(&self) -> Option<usize> {
        let node = self.selected.as_ref()?.node?;
        let is_sel = |&i: &usize| self.snap.entries[i].node == node;
        // Usually the row found last time; a directory can have a million rows.
        let hint = self.sel_hint.get();
        if self.rows.get(hint).is_some_and(is_sel) {
            return Some(hint);
        }
        let found = self.rows.iter().position(is_sel)?;
        self.sel_hint.set(found);
        Some(found)
    }

    pub fn selected_entry(&self) -> Option<&EntryData> {
        self.selected_index()
            .map(|i| &self.snap.entries[self.rows[i]])
    }

    pub(crate) fn select(&mut self, index: usize) {
        if let Some(&i) = self.rows.get(index) {
            self.selected = Some(Selection::of(&self.snap.entries[i]));
            self.sel_hint.set(index);
        }
    }

    /// Selects row `index` as a cursor move of the user's, so re-sorts follow the selected row.
    pub(crate) fn move_to(&mut self, index: usize) {
        self.moved = true;
        self.select(index);
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.moved = true;
        let cur = self.selected_index().unwrap_or(0);
        let next = cur.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.select(next);
    }

    fn save_cursor(&mut self) {
        let c = Cursor {
            selected: self.selected.clone(),
            scroll: self.scroll,
        };
        self.cursors.insert(self.snap.dir_path.clone(), c);
    }

    /// Opens the directory of entry `index` of the snapshot: in tree mode a row at any depth,
    /// and the expanded directories stay expanded.
    pub(super) fn enter(&mut self, index: usize) {
        self.save_cursor();
        let target = crate::state::tree::target_of(&self.snap, index);
        let saved = self.cursors.get(&target.path).cloned();
        self.target = target;
        self.moved = saved.is_some();
        self.selected = saved.as_ref().and_then(|c| c.selected.clone());
        self.scroll = saved.map_or(0, |c| c.scroll);
    }

    pub(super) fn up(&mut self) {
        if self.snap.at_root {
            return;
        }
        self.save_cursor();
        let path = &self.snap.dir_path;
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let name = name.to_vec();
        let saved = self.cursors.get(&parent).cloned();
        self.target = Target {
            node: None,
            path: parent,
            ident: None,
        };
        self.moved = true;
        self.scroll = saved.as_ref().map_or(0, |c| c.scroll);
        // Select the directory we came from, found again by name after the refresh.
        self.selected = Some(Selection::named(name));
    }

    /// Goes to an entry below the scan root `root`: opens its parent directory and selects it.
    pub(crate) fn go_to(&mut self, path: &HostPath, root: &HostPath) {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        if path == root {
            return;
        }
        self.save_cursor();
        self.target = Target {
            node: None,
            path: parent,
            ident: None,
        };
        self.moved = true;
        self.scroll = 0;
        self.selected = Some(Selection::named(name.to_vec()));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::model::snapshot::{Snapshot, test_entry};
    use crate::tests::{snap_with_totals, snap_without_totals};
    use crate::theme::Theme;

    fn dir(i: u32, name: &str, size: u64) -> EntryData {
        EntryData {
            is_dir: true,
            ..test_entry(i, name, size)
        }
    }

    /// A browser of `/r` with the directories `d0` (the largest) to `d4`.
    fn browser() -> (App, Instant) {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let entries = (0..5)
            .map(|i| dir(i, &format!("d{i}"), 100 - u64::from(i)))
            .collect();
        app.refresh(snap_with_totals(entries, 1), t0);
        (app, t0)
    }

    fn selected(app: &App) -> Option<String> {
        app.selected_entry()
            .map(|e| String::from_utf8_lossy(&e.name).into_owned())
    }

    #[test]
    fn moves_stay_within_the_rows() {
        let (mut app, _) = browser();
        assert_eq!(app.selected_index(), Some(0));
        app.move_by(3);
        assert_eq!(selected(&app).as_deref(), Some("d3"));
        app.move_by(10);
        assert_eq!(app.selected_index(), Some(4), "the last row at most");
        app.move_by(-10);
        assert_eq!(app.selected_index(), Some(0), "the first row at least");
        app.move_to(2);
        assert_eq!(selected(&app).as_deref(), Some("d2"));
        // A row that is not there selects nothing new.
        app.select(99);
        assert_eq!(selected(&app).as_deref(), Some("d2"));
    }

    #[test]
    fn the_selection_is_found_where_it_moved_to() {
        let (mut app, t0) = browser();
        app.move_to(3);
        // The same entries, the selected one now the largest.
        let mut entries = app.snap.entries.clone();
        entries[3].size = 1000;
        app.refresh(snap_with_totals(entries, 2), t0);
        app.reorder(t0);
        let at = app.selected_index().expect("still selected");
        assert_eq!(app.snap.entries[app.rows[at]].name, b"d3");
    }

    #[test]
    fn the_cursor_of_a_directory_left_comes_back() {
        let (mut app, t0) = browser();
        app.move_to(2);
        app.enter(app.rows[2]);
        assert_eq!(app.target.path, HostPath::from("/r").join(b"d2"));
        assert!(
            app.selected.is_none(),
            "a directory not seen before starts at the top"
        );
        let inside = Snapshot {
            dir_path: HostPath::from("/r").join(b"d2"),
            at_root: false,
            ..snap_without_totals(vec![dir(10, "inner", 5)], 2)
        };
        app.refresh(inside, t0);
        // Up selects the directory come from, by name until the rows are read.
        app.up();
        assert_eq!(app.target.path, HostPath::from("/r"));
        let sel = app.selected.as_ref().expect("a selection");
        assert_eq!((sel.node, &sel.name[..]), (None, &b"d2"[..]));
    }

    #[test]
    fn go_to_opens_the_parent_and_selects_by_name() {
        let (mut app, _) = browser();
        let root = HostPath::from("/r");
        let path = root.join(b"d1").join(b"deep.bin");
        app.go_to(&path, &root);
        assert_eq!(app.target.path, root.join(b"d1"));
        assert_eq!(
            app.selected.as_ref().map(|s| &s.name[..]),
            Some(&b"deep.bin"[..])
        );
        // The root itself has no parent to open below the scan.
        app.go_to(&root, &root);
        assert_eq!(app.target.path, root.join(b"d1"));
    }
}
