//! What the pane shows of the selected entry, read under the read lock ([`read`]): its facts,
//! its largest children, and the rule it matched or the matches below it by category
//! (`dfdu_session::details`), for the entry the app selects.

use dfdu_engine::Model;
pub use dfdu_session::details::{Facts, Info, Largest, Matched, Subject};
use dfdu_session::details::{LARGEST_MAX_CHILDREN, facts};

use crate::app::App;
use crate::model::snapshot::Mark;

/// The cleanup view's selected item while that view is shown, else the list's selected entry.
pub(super) fn subject(app: &App) -> Option<Subject> {
    if crate::features::cleanup::active(app) {
        let it = crate::features::cleanup::selected(app)?;
        return Some(Subject {
            node: it.node,
            pseudo: false,
            compressed: false,
            mark: if it.scanning {
                Mark::Scanning
            } else {
                Mark::None
            },
        });
    }
    app.session.selected_entry().map(|e| Subject {
        node: e.node,
        pseudo: e.pseudo,
        compressed: e.compressed,
        mark: e.mark,
    })
}

/// Reads the selected entry's details; runs under the read lock. Costs a walk up to the root
/// for the path and, for a directory of at most [`LARGEST_MAX_CHILDREN`] children, a look at
/// each child; never a walk of the subtree.
pub fn read(m: &Model, app: &App) -> Info {
    read_within::<LARGEST_MAX_CHILDREN>(m, app)
}

/// [`read`], listing the largest children only of directories with at most `MAX` children.
pub(super) fn read_within<const MAX: usize>(m: &Model, app: &App) -> Info {
    if !app.details.open {
        return Info::default();
    }
    let Some(e) = subject(app) else {
        return Info::default();
    };
    Info {
        node: Some(e.node),
        facts: facts::<MAX>(m, &e, app.session.size_mode),
    }
}

#[cfg(test)]
mod tests;
