//! What the features read from the engine model besides the directory snapshot
//! (`dfdu_session::extras`), read with the app: with every snapshot, and on its own (without
//! copying the directory again) when a feature needs it sooner, as the session driver plans it
//! (`dfdu_session::extras::plan`). The driver reads them itself; tests read them here.

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
