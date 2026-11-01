//! Folding a new [`Snapshot`] into [`App`]: the session brings the rows and the selection up to
//! date ([`SessionState::refresh`](dfdu_session::SessionState::refresh)); the deleted rows kept
//! as placeholders, the rows moving to their places and the scroll are the terminal's.

use std::time::Instant;

use dfdu_session::state::Reordered;

use super::App;
use crate::model::snapshot::Snapshot;

#[cfg(test)]
mod tests;

impl App {
    pub fn refresh(&mut self, mut snap: Snapshot, now: Instant) {
        self.motion.carry_ghosts(&self.session.snap, &mut snap, now);
        // Deleted rows kept as placeholders hold still until the next re-sort.
        let hold = self.motion.has_ghosts();
        let r = self.session.refresh(snap, now, hold);
        self.reordered(r);
    }

    /// Applies sort requests and the automatic sort to the snapshot already held, for a frame
    /// drawn without reading the engine again.
    pub fn reorder(&mut self, now: Instant) {
        let hold = self.motion.has_ghosts();
        let r = self.session.reorder(now, hold);
        self.reordered(r);
    }

    /// Moves the rows of a re-sort to their new places, and the scroll where the session says.
    fn reordered(&mut self, r: Reordered) {
        if let Some(re) = &r.resorted {
            let s = &self.session;
            (self.motion).resorted(&re.before, &s.list.rows, &s.snap.entries, re.auto);
        }
        self.scroll = r.scroll.apply(self.scroll);
    }
}
