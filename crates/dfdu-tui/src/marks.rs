//! Marked entries: `Space` marks, the footer shows their live total, and a delete acts on them.

use dfdu_engine::Model;
use ratatui::text::Span;

use crate::app::App;
use crate::snapshot::EntryData;

/// The marked entries, in any directory.
#[derive(Clone, Debug, Default)]
pub struct Marks {}

/// What [`read`] found for the marks under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Marked {}

/// Marks or unmarks the selected entry.
pub fn toggle(_app: &mut App) {}

pub fn clear(_app: &mut App) {}

/// The first cell of a row.
pub fn gutter(_app: &App, _e: &EntryData) -> Span<'static> {
    Span::raw(" ")
}

/// How many entries are marked and their total, for the status line.
pub fn summary(_app: &App) -> Option<Span<'static>> {
    None
}

/// True when the marks changed since [`read`] last ran. A read ([`read`], then [`apply`]) must
/// make it false: while it is true, the run loop reads again at once.
pub fn stale(_app: &App) -> bool {
    false
}

/// Resolves the marks in the model; runs under the read lock.
pub fn read(_m: &Model, _app: &App) -> Marked {
    Marked {}
}

/// Takes in what [`read`] found, now in `app.extras.marked`, after every read (with a new
/// snapshot or without): drops the marks that no longer resolve.
pub fn apply(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use crate::app::keys::tests::{keys_change_nothing, press};

    #[test]
    fn space_changes_nothing() {
        keys_change_nothing(&[press(KeyCode::Char(' '))]);
    }
}
