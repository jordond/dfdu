//! The command palette (`:`, `Ctrl-P`): every action and setting, searched by name.

use std::time::Instant;

use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use crate::app::{App, Effect};

/// State of the open palette, held in `Overlay::Palette`.
#[derive(Clone, Debug, Default)]
pub struct Palette {}

pub fn open(_app: &mut App) {}

pub fn key(_app: &mut App, _k: KeyEvent, _now: Instant) -> Vec<Effect> {
    Vec::new()
}

/// Draws the palette in `area` and returns the rectangle it covers.
pub fn draw(_f: &mut Frame, _app: &App, _area: Rect) -> Rect {
    Rect::default()
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::keys::tests::{keys_change_nothing, press};

    #[test]
    fn colon_and_ctrl_p_change_nothing() {
        keys_change_nothing(&[
            press(KeyCode::Char(':')),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ]);
    }
}
