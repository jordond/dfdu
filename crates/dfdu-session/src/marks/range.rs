//! The range mark (`v`), as in ranger, lf and yazi: while it is on, every row the cursor passes
//! in the list is marked. `v` again or `Esc` ends it, and so does leaving the directory or the
//! list. A read-only browser does not start one, as nothing marked could be deleted.
//!
//! It works only while the front end draws the list's rows (`list`).

use super::{add, item_of};
use crate::actions::{Action, first_key};
use crate::state::{READ_ONLY, SessionState};

/// True while a range mark is on in the directory and view shown.
pub fn on(s: &SessionState, list: bool) -> bool {
    s.marks.range.as_ref() == Some(&s.snap.dir_path) && list
}

/// `v`: starts a range mark at the selected row, which it marks, or ends the one that is on.
pub fn toggle(s: &mut SessionState, list: bool) {
    if end(s, list) {
        return;
    }
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return;
    }
    if !list {
        let files = first_key(Action::ViewFiles);
        s.status = Some(format!("a range is marked in Files ({files})"));
        return;
    }
    let Some(sel) = s.selected_index() else {
        return;
    };
    s.marks.range = Some(s.snap.dir_path.clone());
    mark_rows(s, sel, sel);
    s.status = Some(format!(
        "range mark: moving marks every row passed; {} or Esc ends it",
        first_key(Action::RangeMark)
    ));
}

/// Ends the range mark that is on; false when none was.
pub fn end(s: &mut SessionState, list: bool) -> bool {
    let was = on(s, list);
    s.marks.range = None;
    if was {
        s.status = Some("range mark ended".into());
    }
    was
}

/// After a move of the cursor from row `before`: marks the rows from there to the row selected
/// now, both included, while a range mark is on.
pub fn follow(s: &mut SessionState, before: Option<usize>, list: bool) {
    if !on(s, list) {
        s.marks.range = None;
        return;
    }
    let (Some(from), Some(to)) = (before, s.selected_index()) else {
        return;
    };
    mark_rows(s, from.min(to), from.max(to));
    s.status = Some(format!(
        "range mark: {} marked; {} or Esc ends it",
        s.marks.items().len(),
        first_key(Action::RangeMark)
    ));
}

/// Marks the rows `from..=to` of the list. The "shared between entries" row is not an entry,
/// and directories are left out while a type filter shows only some of their files.
fn mark_rows(s: &mut SessionState, from: usize, to: usize) {
    let folders = !s.types_active();
    let items = (s.list.rows.get(from..=to).unwrap_or_default().iter())
        .filter(|&&i| {
            let e = &s.snap.entries[i];
            !e.pseudo && (folders || !e.is_dir)
        })
        .map(|&i| item_of(s, i))
        .collect();
    add(s, items);
}

#[cfg(test)]
mod tests {
    use dfdu_tree::filetype::TypeFilter;

    use super::*;
    use crate::snapshot::{EntryData, test_entry};
    use crate::state::tests::{browser, marked_names};

    /// Files, with the rows a flat list.
    /// The list's rows are drawn.
    const FILES: bool = true;

    /// A browser of `/r` with the directory `d0` (the largest), the files `f1` to `f4`, and the
    /// "shared between entries" row last.
    fn files() -> SessionState {
        let mut entries = vec![EntryData {
            is_dir: true,
            ..test_entry(0, "d0", 100)
        }];
        entries.extend((1..5).map(|i| test_entry(i, &format!("f{i}"), 100 - u64::from(i))));
        entries.push(EntryData {
            pseudo: true,
            ..test_entry(5, "shared", 1)
        });
        browser(entries)
    }

    /// Moves the cursor to row `to` while a range mark follows it.
    fn move_to(s: &mut SessionState, to: usize) {
        let before = s.selected_index();
        s.move_to(to);
        follow(s, before, FILES);
    }

    #[test]
    fn v_marks_the_rows_passed_until_it_ends() {
        let mut s = files();
        s.move_to(1);
        toggle(&mut s, FILES);
        assert!(on(&s, FILES));
        assert_eq!(marked_names(&s), ["f1"]);
        move_to(&mut s, 3);
        assert_eq!(marked_names(&s), ["f1", "f2", "f3"]);
        assert_eq!(
            s.status.as_deref(),
            Some("range mark: 3 marked; v or Esc ends it")
        );
        // Back up over rows marked already, and on past the start.
        move_to(&mut s, 0);
        assert_eq!(marked_names(&s), ["f1", "f2", "f3", "d0"]);
        toggle(&mut s, FILES);
        assert!(!on(&s, FILES));
        assert_eq!(s.status.as_deref(), Some("range mark ended"));
        move_to(&mut s, 4);
        assert_eq!(s.marks.items().len(), 4, "moves after the end mark nothing");
        assert!(!end(&mut s, FILES), "none to end");
    }

    #[test]
    fn rows_that_are_no_entries_or_folders_hidden_by_types_are_left_out() {
        let mut s = files();
        s.move_to(3);
        toggle(&mut s, FILES);
        move_to(&mut s, 5);
        assert_eq!(marked_names(&s), ["f3", "f4"], "not the shared row");
        end(&mut s, FILES);
        crate::marks::clear(&mut s);
        s.types = TypeFilter::parse("video").expect("types");
        s.move_to(0);
        toggle(&mut s, FILES);
        move_to(&mut s, 1);
        assert_eq!(
            marked_names(&s),
            ["f1"],
            "only some of d0's files are shown"
        );
    }

    #[test]
    fn a_range_starts_only_where_the_list_is_shown_in_a_browser_that_deletes() {
        let mut s = files();
        s.read_only = true;
        toggle(&mut s, FILES);
        assert!(!on(&s, FILES));
        assert_eq!(s.status.as_deref(), Some(READ_ONLY));
        s.read_only = false;
        let map = false;
        toggle(&mut s, map);
        assert!(!on(&s, map));
        assert_eq!(s.status.as_deref(), Some("a range is marked in Files (1)"));
        assert!(s.marks.is_empty());
    }
}
