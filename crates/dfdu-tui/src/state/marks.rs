//! Marked entries (`dfdu_session::marks`), read with the app: the marks' functions on the app's
//! session, with whether the view shown draws the list's rows. `v` marks a range as the cursor
//! moves ([`range`]).

use dfdu_engine::Model;

use crate::app::App;
use crate::model::snapshot::EntryData;
pub use dfdu_session::marks::{Item, Marked};

pub(crate) mod range;

/// Marks or unmarks the selected entry and moves the cursor down a row.
pub fn toggle(app: &mut App) {
    dfdu_session::marks::toggle(&mut app.session);
}

/// The mark of entry `index` of the snapshot.
pub(crate) fn item_of(app: &App, index: usize) -> Item {
    dfdu_session::marks::item_of(&app.session, index)
}

/// Marks `item` or unmarks it (found by its node or path).
pub fn set(app: &mut App, item: Item, on: bool) {
    dfdu_session::marks::set(&mut app.session, item, on);
}

/// Marks every one of `items` not marked yet and returns how many it marked.
pub(crate) fn add(app: &mut App, items: Vec<Item>) -> usize {
    dfdu_session::marks::add(&mut app.session, items)
}

pub fn clear(app: &mut App) {
    dfdu_session::marks::clear(&mut app.session);
}

/// True when the row of `e` shows as marked.
pub fn is_marked(app: &App, e: &EntryData) -> bool {
    dfdu_session::marks::is_marked(&app.session, e)
}

/// How many entries are marked and their total, for the status line; `None` when nothing is.
pub fn summary(app: &App) -> Option<String> {
    dfdu_session::marks::summary(&app.session)
}

/// True when the marks changed since [`read`] last ran.
pub fn stale(app: &App) -> bool {
    dfdu_session::marks::stale(&app.session)
}

/// Resolves the marks in the model; runs under the read lock.
pub fn read(m: &Model, app: &App) -> Marked {
    dfdu_session::marks::read(m, &app.session)
}

/// Takes in what [`read`] found, now in `app.extras.marked`.
pub fn apply(app: &mut App) {
    dfdu_session::marks::apply(&mut app.session, &app.extras.marked);
}

/// `*`: inverts the marks of the current directory's entries as the list shows them.
pub fn invert(app: &mut App) {
    let list = crate::app::views::list_shown(app);
    dfdu_session::marks::invert(&mut app.session, list);
}

#[cfg(test)]
mod tests;
