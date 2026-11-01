//! The filter of the current directory (`/`, `dfdu_session::filter`), read with the app: its
//! edits bring the rows up to date at once and run what they ask for.

use std::time::Instant;

use dfdu_session::filter::{self, Edited};

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
use crate::input::line::TextOp;

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

#[cfg(test)]
mod tests;
