//! The largest files below (`5`), as broot's whale mode: the largest files anywhere under the
//! current directory in one flat list, with their paths relative to it.
//!
//! Only while the view is open, the extras read walks the directory's subtree under the read
//! lock and keeps the largest [`TOP`](dfdu_session::largest::TOP) files in a bounded heap. It
//! walks again when the view opens, for another directory or size mode, and when the scan data
//! changed: at once when no scan is running (a delete), and during a scan at most every
//! [`MIN_INTERVAL`](dfdu_session::largest::MIN_INTERVAL) and at most a tenth of the time, as the
//! run loop paces the snapshot; the walk, the files and what the keys ask ([`Intent`]) are in
//! `dfdu_session::largest`. It is one of the review views (`features::review`): `Space` ticks a file (marks it, as in the
//! list), `Enter` shows it in Files, in its directory, and `d` deletes what is ticked.

use dfdu_session::largest as session;
use ratatui::crossterm::event::KeyEvent;

use crate::app::keys;
use crate::app::views;
use crate::app::{App, Effect};
pub use session::File;

mod draw;

pub use draw::draw;

/// What only the terminal keeps of the view between frames: the scroll.
#[derive(Clone, Debug, Default)]
pub struct Largest {
    scroll: usize,
}

/// True when the view is shown, and so takes the keys it knows.
#[cfg(test)]
fn active(app: &App) -> bool {
    session::active(&app.session)
}

/// Keys the view takes before the action registry; `None` passes the key on. The keys that
/// delete act on what is marked, never on the list's selection hidden behind the view.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let action = keys::action_for(k)?;
    let intent = session::intent(&app.session, action)?;
    let then = session::act(&mut app.session, intent);
    Some(views::carry(app, then))
}

#[cfg(test)]
mod tests;
