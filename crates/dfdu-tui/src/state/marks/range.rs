//! The range mark (`v`, `dfdu_session::marks::range`), with whether the view shown draws the
//! list's rows.

use dfdu_session::marks::range;

use crate::app::App;
use crate::app::views::list_shown;

/// True while a range mark is on in the directory and view shown.
#[cfg(test)]
pub(crate) fn on(app: &App) -> bool {
    range::on(&app.session, list_shown(app))
}

/// `v`: starts a range mark at the selected row, which it marks, or ends the one that is on.
pub fn toggle(app: &mut App) {
    let list = list_shown(app);
    range::toggle(&mut app.session, list);
}

/// Ends the range mark that is on; false when none was.
pub fn end(app: &mut App) -> bool {
    let list = list_shown(app);
    range::end(&mut app.session, list)
}

/// After a move of the cursor from row `before`: marks the rows passed while a range mark is
/// on.
pub fn follow(app: &mut App, before: Option<usize>) {
    let list = list_shown(app);
    range::follow(&mut app.session, before, list);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::app::keys::READ_ONLY;
    use crate::model::snapshot::{EntryData, test_entry};
    use crate::tests::snap_with_totals;
    use crate::theme::Theme;

    #[test]
    fn a_range_starts_only_in_files_of_a_browser_that_deletes() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mut entries = vec![EntryData {
            is_dir: true,
            ..test_entry(0, "d0", 100)
        }];
        entries.extend((1..5).map(|i| test_entry(i, &format!("f{i}"), 100 - u64::from(i))));
        app.refresh(snap_with_totals(entries, 1), t0);
        app.session.read_only = true;
        toggle(&mut app);
        assert!(!on(&app));
        assert_eq!(app.session.status.as_deref(), Some(READ_ONLY));
        app.session.read_only = false;
        crate::app::views::switch(&mut app, crate::app::views::View::Map);
        // As a frame with room for the map sets it.
        app.treemap.shown = true;
        toggle(&mut app);
        assert!(!on(&app));
        assert_eq!(
            app.session.status.as_deref(),
            Some("a range is marked in Files (1)")
        );
        assert!(app.session.marks.is_empty());
    }
}
