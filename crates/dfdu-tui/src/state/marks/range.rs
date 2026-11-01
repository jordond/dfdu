//! The range mark (`v`, `dfdu_session::marks::range`), read with the app in tests.

use dfdu_session::marks::range;

use crate::app::App;

/// True while a range mark is on in the directory and view shown.
pub(crate) fn on(app: &App) -> bool {
    range::on(&app.session)
}

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
        range::toggle(&mut app.session);
        assert!(!on(&app));
        assert_eq!(app.session.status.as_deref(), Some(READ_ONLY));
        app.session.read_only = false;
        crate::app::views::switch(&mut app, crate::app::views::View::Map);
        // As a frame with room for the map sets it.
        app.session.set_map_drawn(true);
        range::toggle(&mut app.session);
        assert!(!on(&app));
        assert_eq!(
            app.session.status.as_deref(),
            Some("a range is marked in Files (1)")
        );
        assert!(app.session.marks.is_empty());
    }
}
