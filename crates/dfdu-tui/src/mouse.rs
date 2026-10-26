//! Mouse input: clicks, double clicks and the wheel, mapped through `App::areas`.

use std::time::Instant;

use ratatui::crossterm::event::MouseEvent;

use crate::app::{App, Effect};

#[derive(Clone, Debug, Default)]
pub struct Mouse {}

/// Handles a mouse event; `None` when it changed nothing, so no frame is drawn.
pub fn event(_app: &mut App, _m: MouseEvent, _now: Instant) -> Option<Vec<Effect>> {
    None
}
