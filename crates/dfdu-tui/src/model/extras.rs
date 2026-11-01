//! What the features read from the engine model besides the directory snapshot
//! (`dfdu_session::extras`), read with the app: with every snapshot, and on its own (without
//! copying the directory again) when a feature needs it sooner, as the run loop plans it
//! ([`plan`]).

use dfdu_engine::Model;
use dfdu_scan::Budget;
pub use dfdu_session::extras::Extras;

use crate::app::App;
use crate::features::debug;

/// Runs under the read lock ([`dfdu_session::extras::read`]).
pub fn read(m: &Model, app: &App, budget: &Budget, counters: debug::Counters) -> Extras {
    dfdu_session::extras::read(m, &app.session, budget, counters)
}

/// Stores what [`read`] found in the session and lets the features take it in; the details pane
/// scrolls to its top for another entry. Called after every read, after [`App::refresh`] when
/// the read brought a new snapshot.
pub fn apply(app: &mut App, extras: Extras) {
    let applied = dfdu_session::extras::apply(&mut app.session, extras);
    if applied.details_moved {
        app.details.scroll = 0;
    }
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
