//! What the features read from the engine model besides the directory snapshot: the marks, the
//! details of the selected entry, the debug counters and the treemap's lower levels. Read with every snapshot, and on its
//! own (without copying the directory again) when a feature needs it sooner.

use dfdu_engine::Model;
use dfdu_scan::Budget;

use crate::app::App;
use crate::{cleanup, debug, details, marks, treemap};

#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub marked: marks::Marked,
    pub details: details::Info,
    pub debug: debug::Info,
    pub treemap: treemap::Info,
    pub cleanup: cleanup::Info,
}

/// Runs under the read lock; costs no walk of the directory or the tree. `counters` are the
/// engine's own, read under the same lock ([`debug::Counters::of`]).
pub fn read(m: &Model, app: &App, budget: &Budget, counters: debug::Counters) -> Extras {
    Extras {
        marked: marks::read(m, app),
        details: details::read(m, app),
        debug: debug::read(m, budget, counters),
        treemap: treemap::read(m, app),
        cleanup: cleanup::read(m, app),
    }
}

/// Stores what [`read`] found in `app.extras` and lets the features take it in. Called after
/// every read, after [`App::refresh`] when the read brought a new snapshot.
pub fn apply(app: &mut App, extras: Extras) {
    app.extras = extras;
    marks::apply(app);
    details::apply(app);
    cleanup::apply(app);
}

/// What the run loop reads from the model before a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Read {
    Nothing,
    /// The extras only.
    Extras,
    /// A snapshot of the directory, and the extras with it.
    Full,
}

/// `full`: a snapshot is due (scan data, or another directory or size mode). `stale`:
/// [`App::extras_stale`].
pub fn plan(full: bool, stale: bool) -> Read {
    match (full, stale) {
        (true, _) => Read::Full,
        (false, true) => Read::Extras,
        (false, false) => Read::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extras_are_read_only_when_stale() {
        assert_eq!(plan(false, false), Read::Nothing);
        assert_eq!(plan(false, true), Read::Extras);
        // A snapshot read brings the extras along; the loop reads them again only when the
        // refresh moved the selection.
        assert_eq!(plan(true, false), Read::Full);
        assert_eq!(plan(true, true), Read::Full);
    }
}
