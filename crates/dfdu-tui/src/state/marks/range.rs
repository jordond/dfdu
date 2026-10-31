//! The range mark (`v`), as in ranger, lf and yazi: while it is on, every row the cursor passes
//! in the list is marked. `v` again or `Esc` ends it, and so does leaving the directory or the
//! list. A read-only browser does not start one, as nothing marked could be deleted.

use super::{add, item_of};
use crate::app::App;
use crate::app::keys::{Action, READ_ONLY, first_key};
use crate::app::views::list_shown;

/// True while a range mark is on in the directory and view shown.
pub fn on(app: &App) -> bool {
    app.marks.range.as_ref() == Some(&app.snap.dir_path) && list_shown(app)
}

/// `v`: starts a range mark at the selected row, which it marks, or ends the one that is on.
pub fn toggle(app: &mut App) {
    if end(app) {
        return;
    }
    if app.read_only {
        app.status = Some(READ_ONLY.into());
        return;
    }
    if !list_shown(app) {
        let list = first_key(Action::ViewFiles);
        app.status = Some(format!("a range is marked in Files ({list})"));
        return;
    }
    let Some(sel) = app.selected_index() else {
        return;
    };
    app.marks.range = Some(app.snap.dir_path.clone());
    mark_rows(app, sel, sel);
    app.status = Some(format!(
        "range mark: moving marks every row passed; {} or Esc ends it",
        first_key(Action::RangeMark)
    ));
}

/// Ends the range mark that is on; false when none was.
pub fn end(app: &mut App) -> bool {
    let was = on(app);
    app.marks.range = None;
    if was {
        app.status = Some("range mark ended".into());
    }
    was
}

/// After a move of the cursor from row `before`: marks the rows from there to the row selected
/// now, both included, while a range mark is on.
pub fn follow(app: &mut App, before: Option<usize>) {
    if !on(app) {
        app.marks.range = None;
        return;
    }
    let (Some(from), Some(to)) = (before, app.selected_index()) else {
        return;
    };
    mark_rows(app, from.min(to), from.max(to));
    app.status = Some(format!(
        "range mark: {} marked; {} or Esc ends it",
        app.marks.items().len(),
        first_key(Action::RangeMark)
    ));
}

/// Marks the rows `from..=to` of the list. The "shared between entries" row is not an entry,
/// and directories are left out while a type filter shows only some of their files.
fn mark_rows(app: &mut App, from: usize, to: usize) {
    let folders = !crate::overlay::type_filter::active(app);
    let items = (app.list.rows.get(from..=to).unwrap_or_default().iter())
        .filter(|&&i| {
            let e = &app.snap.entries[i];
            !e.pseudo && (folders || !e.is_dir)
        })
        .map(|&i| item_of(app, i))
        .collect();
    add(app, items);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_tree::filetype::TypeFilter;

    use super::*;
    use crate::model::snapshot::{EntryData, test_entry};
    use crate::tests::{marked_names, snap_with_totals};
    use crate::theme::Theme;

    /// A browser of `/r` with the directory `d0` (the largest), the files `f1` to `f4`, and the
    /// "shared between entries" row last.
    fn browser() -> App {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut entries = vec![EntryData {
            is_dir: true,
            ..test_entry(0, "d0", 100)
        }];
        entries.extend((1..5).map(|i| test_entry(i, &format!("f{i}"), 100 - u64::from(i))));
        entries.push(EntryData {
            pseudo: true,
            ..test_entry(5, "shared", 1)
        });
        app.refresh(snap_with_totals(entries, 1), t0);
        app
    }

    /// Moves the cursor to row `to` while a range mark follows it.
    fn move_to(app: &mut App, to: usize) {
        let before = app.selected_index();
        app.move_to(to);
        follow(app, before);
    }

    #[test]
    fn v_marks_the_rows_passed_until_it_ends() {
        let mut app = browser();
        app.move_to(1);
        toggle(&mut app);
        assert!(on(&app));
        assert_eq!(marked_names(&app), ["f1"]);
        move_to(&mut app, 3);
        assert_eq!(marked_names(&app), ["f1", "f2", "f3"]);
        assert_eq!(
            app.status.as_deref(),
            Some("range mark: 3 marked; v or Esc ends it")
        );
        // Back up over rows marked already, and on past the start.
        move_to(&mut app, 0);
        assert_eq!(marked_names(&app), ["f1", "f2", "f3", "d0"]);
        toggle(&mut app);
        assert!(!on(&app));
        assert_eq!(app.status.as_deref(), Some("range mark ended"));
        move_to(&mut app, 4);
        assert_eq!(
            app.marks.items().len(),
            4,
            "moves after the end mark nothing"
        );
        assert!(!end(&mut app), "none to end");
    }

    #[test]
    fn rows_that_are_no_entries_or_folders_hidden_by_types_are_left_out() {
        let mut app = browser();
        app.move_to(3);
        toggle(&mut app);
        move_to(&mut app, 5);
        assert_eq!(marked_names(&app), ["f3", "f4"], "not the shared row");
        end(&mut app);
        crate::state::marks::clear(&mut app);
        app.types = TypeFilter::parse("video").expect("types");
        app.move_to(0);
        toggle(&mut app);
        move_to(&mut app, 1);
        assert_eq!(
            marked_names(&app),
            ["f1"],
            "only some of d0's files are shown"
        );
    }

    #[test]
    fn a_range_starts_only_in_files_of_a_browser_that_deletes() {
        let mut app = browser();
        app.read_only = true;
        toggle(&mut app);
        assert!(!on(&app));
        assert_eq!(app.status.as_deref(), Some(READ_ONLY));
        app.read_only = false;
        crate::app::views::switch(&mut app, crate::app::views::View::Map);
        // As a frame with room for the map sets it.
        app.treemap.shown = true;
        toggle(&mut app);
        assert!(!on(&app));
        assert_eq!(
            app.status.as_deref(),
            Some("a range is marked in Files (1)")
        );
        assert!(app.marks.is_empty());
    }
}
