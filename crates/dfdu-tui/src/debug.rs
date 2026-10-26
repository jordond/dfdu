//! The debug panel (`F12`): pipeline counters, the byte budget, lock times and frame costs.

use std::time::Duration;

use dfdu_engine::Model;
use dfdu_scan::Budget;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::App;

#[derive(Clone, Debug, Default)]
pub struct Debug {
    pub open: bool,
}

/// What [`read`] copied under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Info {}

/// Opens or closes the panel.
pub fn toggle(_app: &mut App) {}

/// Draws the panel over the list area `area`, if it is open.
pub fn draw(_f: &mut Frame, _app: &App, _area: Rect) {}

/// Copies the counters; runs under the read lock. Cheap when the panel is not `open`.
pub fn read(_m: &Model, _budget: &Budget, _open: bool) -> Info {
    Info {}
}

/// Records how long the last frame took to draw and its model read held the lock.
pub fn frame_drawn(_app: &mut App, _draw: Duration, _read: Duration) {}

/// The panel's numbers as text, for `--debug-stats`.
pub fn summary(_m: &Model, _budget: &Budget) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use crate::app::keys::tests::{keys_change_nothing, press};

    #[test]
    fn f12_changes_nothing() {
        keys_change_nothing(&[press(KeyCode::F(12))]);
    }
}
