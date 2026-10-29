//! The range mark (`v`), as in ranger, lf and yazi: while it is on, every row the cursor passes
//! in the list is marked. `v` again or `Esc` ends it, and so does leaving the directory or the
//! list. A read-only browser does not start one, as nothing marked could be deleted.

use super::{add, item_of};
use crate::app::App;
use crate::app::keys::{Action, READ_ONLY, first_key};
use crate::features::{cleanup, dupes, largest, treemap};

/// True when the list is the view shown, where the range mark works.
fn list_shown(app: &App) -> bool {
    !treemap::active(app) && !cleanup::active(app) && !dupes::active(app) && !largest::active(app)
}

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
    let items = (app.rows.get(from..=to).unwrap_or_default().iter())
        .filter(|&&i| {
            let e = &app.snap.entries[i];
            !e.pseudo && (folders || !e.is_dir)
        })
        .map(|&i| item_of(app, i))
        .collect();
    add(app, items);
}
