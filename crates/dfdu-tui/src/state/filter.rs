//! The filter of the current directory (`/`, `dfdu_session::filter`), read with the app: its
//! edits bring the rows up to date at once and run what they ask for.

use std::time::Instant;

use dfdu_session::filter::{self, Edited};

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
use crate::input::line::TextOp;

/// Starts editing the filter, with the cursor after the query in force.
pub fn open(app: &mut App) {
    filter::open(&mut app.session);
}

/// True while the filter input takes the keys.
pub fn editing(app: &App) -> bool {
    filter::editing(&app.session)
}

/// An edit of the query while editing: typing filters at once, `Enter` keeps the filter (and
/// opens the only entry left, when that is a directory), `Esc` clears it.
pub fn edit(app: &mut App, op: TextOp, now: Instant) -> Vec<Effect> {
    match filter::edit(&mut app.session, op) {
        Edited::Refilter => {
            let before = filter::selected_node(&app.session);
            app.session.request_refilter();
            app.reorder(now);
            filter::refiltered(&mut app.session, before);
        }
        Edited::Enter => return keys::run(app, Action::Enter),
        Edited::Done => {}
    }
    Vec::new()
}

/// Clears an active filter. False if there was none.
pub fn clear(app: &mut App) -> bool {
    filter::clear(&mut app.session)
}

#[cfg(test)]
mod tests;
