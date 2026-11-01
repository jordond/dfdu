//! What the front end laid out in its last frame, as it tells the session: the rows the view
//! shown had room for ([`SessionState::set_viewport`]), and whether the Map view drew the
//! treemap or, without room for it, the list ([`SessionState::set_map_drawn`]). The session
//! never reads screen geometry; paging, sorting the first screen of rows and what acts on the
//! list's rows use these hints.

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
    /// The last frame drew the treemap: the Map view was shown with room for it.
    pub map: bool,
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

    /// The front end drew the treemap in its last frame, or not.
    pub fn set_map_drawn(&mut self, drawn: bool) {
        self.viewport.map = drawn;
    }

    /// True when the treemap is on and was drawn last frame, so it takes the keys and clicks
    /// that act on its tiles.
    pub fn map_drawn(&self) -> bool {
        crate::treemap::on(self) && self.viewport.map
    }

    /// The list's rows are drawn, where the range mark and inverting the marks work: in Files,
    /// and in Map while the last frame had no room for the map.
    pub fn list_shown(&self) -> bool {
        match self.current_view() {
            View::Files => true,
            View::Map => !self.map_drawn(),
            View::Cleanup | View::Duplicates | View::Largest => false,
        }
    }
}
