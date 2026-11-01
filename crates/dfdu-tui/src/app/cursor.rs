//! The cursor moves into and out of directories with the list's scroll: the session saves the
//! selection of each directory left (`SessionState::enter`, `up`, `go_to`) and the app the
//! scroll with it, to restore both on the way back.

use dfdu_session::state::Moved;
use dfdu_tree::HostPath;

use super::App;

impl App {
    /// Keeps the scroll of the directory left, and puts back that of the directory opened.
    pub(crate) fn moved(&mut self, m: Moved) {
        self.scrolls.insert(m.left, self.scroll);
        if let Some(dir) = m.opened {
            self.scroll = self.scrolls.get(&dir).copied().unwrap_or(0);
        }
    }

    /// Goes to an entry below the scan root `root`: opens its parent directory and selects it,
    /// with the list scrolled to the top.
    pub(crate) fn go_to(&mut self, path: &HostPath, root: &HostPath) {
        if let Some(m) = self.session.go_to(path, root) {
            self.went(m);
        }
    }

    /// After the session went to an entry in another directory: keeps the scroll of the
    /// directory left, and shows the one opened from the top.
    pub(crate) fn went(&mut self, m: Moved) {
        self.scrolls.insert(m.left, self.scroll);
        self.scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::model::snapshot::{EntryData, Snapshot, test_entry};
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
        app.session
            .selected_entry()
            .map(|e| String::from_utf8_lossy(&e.name).into_owned())
    }

    #[test]
    fn moves_stay_within_the_rows() {
        let (mut app, _) = browser();
        assert_eq!(app.session.selected_index(), Some(0));
        app.session.move_by(3);
        assert_eq!(selected(&app).as_deref(), Some("d3"));
        app.session.move_by(10);
        assert_eq!(
            app.session.selected_index(),
            Some(4),
            "the last row at most"
        );
        app.session.move_by(-10);
        assert_eq!(
            app.session.selected_index(),
            Some(0),
            "the first row at least"
        );
        app.session.move_to(2);
        assert_eq!(selected(&app).as_deref(), Some("d2"));
        // A row that is not there selects nothing new.
        app.session.select(99);
        assert_eq!(selected(&app).as_deref(), Some("d2"));
    }

    #[test]
    fn the_selection_is_found_where_it_moved_to() {
        let (mut app, t0) = browser();
        app.session.move_to(3);
        // The same entries, the selected one now the largest.
        let mut entries = app.session.snap.entries.clone();
        entries[3].size = 1000;
        app.refresh(snap_with_totals(entries, 2), t0);
        app.reorder(t0);
        let at = app.session.selected_index().expect("still selected");
        assert_eq!(
            app.session.snap.entries[app.session.list.rows[at]].name,
            b"d3"
        );
    }

    #[test]
    fn the_cursor_of_a_directory_left_comes_back() {
        let (mut app, t0) = browser();
        app.session.move_to(2);
        crate::app::keys::run(&mut app, crate::app::keys::Action::Enter);
        assert_eq!(app.session.target.path, HostPath::from("/r").join(b"d2"));
        assert!(
            app.session.selected.is_none(),
            "a directory not seen before starts at the top"
        );
        let inside = Snapshot {
            dir_path: HostPath::from("/r").join(b"d2"),
            at_root: false,
            ..snap_without_totals(vec![dir(10, "inner", 5)], 2)
        };
        app.refresh(inside, t0);
        // Up selects the directory come from, by name until the rows are read.
        crate::app::keys::run(&mut app, crate::app::keys::Action::Parent);
        assert_eq!(app.session.target.path, HostPath::from("/r"));
        let sel = app.session.selected.as_ref().expect("a selection");
        assert_eq!((sel.node, &sel.name[..]), (None, &b"d2"[..]));
    }

    #[test]
    fn go_to_opens_the_parent_and_selects_by_name() {
        let (mut app, _) = browser();
        let root = HostPath::from("/r");
        let path = root.join(b"d1").join(b"deep.bin");
        app.go_to(&path, &root);
        assert_eq!(app.session.target.path, root.join(b"d1"));
        assert_eq!(
            app.session.selected.as_ref().map(|s| &s.name[..]),
            Some(&b"deep.bin"[..])
        );
        // The root itself has no parent to open below the scan.
        app.go_to(&root, &root);
        assert_eq!(app.session.target.path, root.join(b"d1"));
    }
}
