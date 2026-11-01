//! What the pane shows of the selected entry, read under the read lock
//! (`dfdu_session::details`): its facts, its largest children, and the rule it matched or the
//! matches below it by category, for the entry the session selects.

pub use dfdu_session::details::{Facts, Largest, Matched};
#[cfg(test)]
use {
    crate::app::App,
    dfdu_engine::Model,
    dfdu_session::details::{Info, LARGEST_MAX_CHILDREN},
};

/// Reads the selected entry's details ([`dfdu_session::details::read`]).
#[cfg(test)]
pub(super) fn read(m: &Model, app: &App) -> Info {
    read_within::<LARGEST_MAX_CHILDREN>(m, app)
}

/// [`read`], listing the largest children only of directories with at most `MAX` children.
#[cfg(test)]
pub(super) fn read_within<const MAX: usize>(m: &Model, app: &App) -> Info {
    dfdu_session::details::read_within::<MAX>(m, &app.session)
}

#[cfg(test)]
mod tests;
