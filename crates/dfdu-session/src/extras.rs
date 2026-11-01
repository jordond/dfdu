//! What the features read from the engine model besides the directory snapshot: the marks, the
//! details of the selected entry, the treemap's lower levels and the cleanup matches. Read with every snapshot, and on its own
//! (without copying the directory again) when a feature needs it sooner ([`stale`]).

use dfdu_engine::Model;

use crate::marks::{self, Marked};
use crate::state::SessionState;
use crate::treemap;
use crate::{cleanup, details};

#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub marked: Marked,
    pub details: details::Info,
    pub treemap: treemap::Info,
    pub cleanup: cleanup::Info,
}

/// Runs under the read lock; costs no walk of the directory or the tree.
pub fn read(m: &Model, s: &SessionState) -> Extras {
    Extras {
        marked: marks::read(m, s),
        details: details::read(m, s),
        treemap: treemap::read(m, s),
        cleanup: cleanup::read(m, s),
    }
}

/// What taking in a read changed that a front end's view state follows.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[must_use]
pub struct Applied {
    /// The details are of another entry than the last read's: they show from their top.
    pub details_moved: bool,
}

/// Stores what [`read`] found in `s.extras` and lets the features take it in. Called after
/// every read, after the refresh when the read brought a new snapshot.
pub fn apply(s: &mut SessionState, extras: Extras) -> Applied {
    s.extras = extras;
    let marked = std::mem::take(&mut s.extras.marked);
    marks::apply(s, &marked);
    s.extras.marked = marked;
    let details_moved = details::apply(s);
    cleanup::apply(s);
    Applied { details_moved }
}

/// True when the features need a read of the engine model for the next frame although no new
/// snapshot is due. A read must make it false: while it is true, the front end reads again at
/// once.
pub fn stale(s: &SessionState) -> bool {
    marks::stale(s) || details::stale(s) || treemap::stale(s) || cleanup::stale(s)
}
