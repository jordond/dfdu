//! What the scan did not enter, in `dfdu-session` (`dfdu_session::outside`): the lookups and
//! their wording are there; here they are read with the app's snapshot and keys, and the
//! cleanup view's lines about links out of the scan are built.

pub(crate) use dfdu_engine::{linked_note, rel_text, tilde};
use dfdu_rules::RuleSet;
use dfdu_tree::HostPath;
use ratatui::text::Line;

use crate::app::App;

/// Most links out of the scan the cleanup view names.
const VIEW_MAX: usize = 3;

/// Starts the lookups the snapshot held needs ([`Outside::request`]).
pub fn request(app: &mut App) {
    app.session.outside.request(&app.session.snap);
}

/// What the details pane says about the mount at `path`, which the scan did not enter
/// ([`Outside::pane_info`]), and the key that scans it.
#[must_use]
pub fn pane_lines(app: &App, path: &HostPath) -> (Option<String>, String) {
    let info = app.session.outside.pane_info(path);
    let key = crate::app::keys::first_key(crate::app::keys::Action::EnterMount);
    (info, format!("{key} scans it"))
}

/// The note after the name of the row of entry `index`, a mount that was not entered.
#[must_use]
pub fn row_note(app: &App, index: usize) -> String {
    let note = app.session.outside.row_note(&app.session.snap, index);
    let key = crate::app::keys::first_key(crate::app::keys::Action::EnterMount);
    format!("  {note}: {key} scans it")
}

/// The cleanup view's lines about rule locations behind links out of the scan.
#[must_use]
pub fn view_lines(app: &App, rules: &RuleSet) -> Vec<Line<'static>> {
    let linked = app.session.outside.linked();
    let mut out: Vec<Line> = linked
        .iter()
        .take(VIEW_MAX)
        .map(|l| {
            let note = linked_note(rules, l, app.session.home.as_deref(), "dfdu");
            Line::styled(format!(" {note}"), app.theme.fg_dim)
        })
        .collect();
    if let Some(more) = dfdu_engine::more_links(linked.len(), VIEW_MAX) {
        out.push(Line::styled(format!(" {more}"), app.theme.fg_dim));
    }
    out
}

#[cfg(test)]
mod tests;
