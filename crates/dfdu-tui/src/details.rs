//! The details pane (`i`): everything the tree knows about the selected entry.

use dfdu_engine::Model;
use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use crate::app::{App, Effect};

#[derive(Clone, Debug, Default)]
pub struct Details {}

/// What [`read`] found for the selected entry under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Info {}

/// Opens or closes the pane.
pub fn toggle(_app: &mut App) {}

/// Moves the focus between the list and the pane.
pub fn focus(_app: &mut App) {}

/// True while the pane has the focus.
pub fn focused(_app: &App) -> bool {
    false
}

/// Handles a key while the pane has the focus; `None` passes it on to the list.
pub fn key(_app: &mut App, _k: &KeyEvent) -> Option<Vec<Effect>> {
    None
}

/// Splits the area below the header into the list and the pane, if the pane is open and fits.
pub fn split(_app: &mut App, area: Rect) -> (Rect, Option<Rect>) {
    (area, None)
}

pub fn draw(_f: &mut Frame, _app: &App, _area: Rect) {}

/// True when the pane shows something other than what [`read`] last read. A read ([`read`],
/// then [`apply`]) must make it false: while it is true, the run loop reads again at once.
pub fn stale(_app: &App) -> bool {
    false
}

/// Reads the selected entry's details; runs under the read lock.
pub fn read(_m: &Model, _app: &App) -> Info {
    Info {}
}

/// Takes in what [`read`] found, now in `app.extras.details`, after every read (with a new
/// snapshot or without).
pub fn apply(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use crate::app::keys::tests::{keys_change_nothing, press};

    #[test]
    fn i_and_tab_change_nothing() {
        keys_change_nothing(&[press(KeyCode::Char('i')), press(KeyCode::Tab)]);
    }
}
