//! Marked entries (`dfdu_session::marks`), read with the app: the marks' functions on the app's
//! session. `v` marks a range as the cursor moves (`dfdu_session::marks::range`).

use crate::app::App;
use crate::model::snapshot::EntryData;
pub use dfdu_session::marks::Item;

#[cfg(test)]
pub(crate) mod range;

/// True when the row of `e` shows as marked.
pub fn is_marked(app: &App, e: &EntryData) -> bool {
    dfdu_session::marks::is_marked(&app.session, e)
}

/// How many entries are marked and their total, for the status line; `None` when nothing is.
pub fn summary(app: &App) -> Option<String> {
    dfdu_session::marks::summary(&app.session)
}

/// `*`: inverts the marks of the current directory's entries as the list shows them.
pub fn invert(app: &mut App) {
    dfdu_session::marks::invert(&mut app.session);
}

#[cfg(test)]
mod tests;
