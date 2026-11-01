//! What the front end laid out in its last frame, as it tells the session: the rows the view
//! shown had room for ([`SessionState::set_viewport`]). The session never reads screen
//! geometry; paging and sorting the first screen of rows use these hints.

use super::SessionState;
use crate::view::View;

/// The facts of the last frame's layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Viewport {
    /// The view whose rows were laid out last.
    pub view: View,
    /// Rows it had room for; zero before the first frame, when no rows are on screen to keep
    /// sorted.
    pub rows: usize,
}

impl SessionState {
    /// The front end laid out `view` with room for `visible_rows` rows.
    pub fn set_viewport(&mut self, view: View, visible_rows: usize) {
        self.viewport.view = view;
        self.viewport.rows = visible_rows;
    }

    /// The rows the cursor moves by a page: those the last frame had room for, at least one.
    pub fn page_rows(&self) -> usize {
        self.viewport.rows.max(1)
    }
}
