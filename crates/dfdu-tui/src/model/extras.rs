//! What the features read from the engine model besides the directory snapshot: the session's
//! (`dfdu_session::extras`: the marks, the treemap's lower levels, the cleanup matches), the
//! details of the selected entry, the debug counters, the nodes of the duplicate files and the
//! largest files below. Read with every snapshot, and on its own (without copying the directory
//! again) when a feature needs it sooner.

use dfdu_engine::Model;
use dfdu_scan::Budget;

use crate::app::App;
use crate::features::{debug, details, largest};

#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub details: details::Info,
    pub debug: debug::Info,
    /// The node of each file of the duplicates found, by path; `None` once a file is gone.
    pub dupes: std::collections::HashMap<std::path::PathBuf, Option<dfdu_tree::NodeRef>>,
    /// A new walk for the largest files below, when one was due.
    pub largest: Option<largest::Info>,
    /// How many files a comparison of duplicates would read, when they were counted, and the
    /// tree's version then.
    pub dupe_candidates: Option<(u64, usize)>,
}

/// Runs under the read lock; costs no walk of the directory or the tree, but for the largest
/// files below while that view is open, at most a tenth of the time ([`largest::read`]).
/// `counters` are the engine's own, read under the same lock ([`debug::Counters::of`]).
pub fn read(m: &Model, app: &App, budget: &Budget, counters: debug::Counters) -> Found {
    let session = dfdu_session::extras::read(m, &app.session);
    let extras = Extras {
        details: details::read(m, app),
        debug: debug::read(m, budget, counters),
        dupes: crate::features::dupes::read(m, app),
        largest: largest::read(m, app),
        dupe_candidates: crate::features::dupes::count(m, app),
    };
    Found { session, extras }
}

/// What [`read`] found: the session's part and the rest.
pub struct Found {
    pub session: dfdu_session::extras::Extras,
    pub extras: Extras,
}

/// Stores what [`read`] found in `app.extras` and lets the features take it in. Called after
/// every read, after [`App::refresh`] when the read brought a new snapshot.
pub fn apply(app: &mut App, Found { session, extras }: Found) {
    dfdu_session::extras::apply(&mut app.session, session);
    app.extras = extras;
    details::apply(app);
    largest::apply(app);
    crate::features::dupes::apply(app);
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
