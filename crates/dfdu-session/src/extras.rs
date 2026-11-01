//! What the features read from the engine model besides the directory snapshot: the marks, the
//! treemap's lower levels and the cleanup matches. Read with every snapshot, and on its own
//! (without copying the directory again) when a feature needs it sooner ([`stale`]).

use dfdu_engine::Model;

use crate::cleanup;
use crate::marks::{self, Marked};
use crate::state::SessionState;
use crate::treemap;

#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub marked: Marked,
    pub treemap: treemap::Info,
    pub cleanup: cleanup::Info,
}

/// Runs under the read lock; costs no walk of the directory or the tree.
pub fn read(m: &Model, s: &SessionState) -> Extras {
    Extras {
        marked: marks::read(m, s),
        treemap: treemap::read(m, s),
        cleanup: cleanup::read(m, s),
    }
}

/// Stores what [`read`] found in `s.extras` and lets the features take it in. Called after
/// every read, after the refresh when the read brought a new snapshot.
pub fn apply(s: &mut SessionState, extras: Extras) {
    s.extras = extras;
    let marked = std::mem::take(&mut s.extras.marked);
    marks::apply(s, &marked);
    s.extras.marked = marked;
    cleanup::apply(s);
}

/// True when the features need a read of the engine model for the next frame although no new
/// snapshot is due. A read must make it false: while it is true, the front end reads again at
/// once.
pub fn stale(s: &SessionState) -> bool {
    marks::stale(s) || treemap::stale(s) || cleanup::stale(s)
}
