//! The marks drawn ([`crate::state::marks`]): the gutter of a list row, and the summary on the
//! status line.

use ratatui::text::Span;

use crate::app::App;
use crate::model::snapshot::EntryData;
use crate::state::marks;

/// The first cell of a row: `●` when the entry is marked.
pub fn gutter(app: &App, e: &EntryData) -> Span<'static> {
    if marks::is_marked(app, e) {
        Span::styled("●", app.theme.mark)
    } else {
        Span::raw(" ")
    }
}

/// How many entries are marked and their total, for the status line.
pub fn summary(app: &App) -> Option<Span<'static>> {
    marks::summary(app).map(|text| Span::styled(text, app.theme.mark))
}
