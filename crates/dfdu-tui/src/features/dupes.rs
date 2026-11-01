//! The Duplicates view (`4`): files with identical contents below the scan root, found on
//! request with [`dfdu_dupes`] once the scan is done; one of the review views
//! (`features::review`).
//!
//! Switching the view on compares nothing: it says how many files a comparison would read
//! ([`count`]), and `Enter` (or `R`) starts one in a thread of its own, from the candidates read
//! under the read lock ([`Effect::FindDupes`]); the view shows its progress and then the groups,
//! the most space to free first, clones last and closed. Ticks are the list's marks: `Space`
//! ticks a copy, or on a group every copy but the one kept, `A` does that for every group, and
//! `d` and `D` delete what is ticked through the usual dialog, which checks each file against
//! the scan. The copy kept is the oldest, and `K` cycles the rule ([`Keep`]); `R` compares
//! again, and leaving the view (or quitting) stops a comparison that runs. Groups of APFS
//! clones, which free nothing, are listed but `A` leaves them alone. The comparison's thread,
//! the keep rules, the rows and what the keys ask ([`Intent`]) are in `dfdu_session::dupes`;
//! the scroll stays here.

use std::collections::HashSet;
use std::path::PathBuf;

#[cfg(test)]
use dfdu_dupes::Found;
use dfdu_dupes::{Candidate, Group};
use dfdu_session::dupes as session;
use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::keys::{self, Action};
use crate::app::views;
use crate::app::{App, Effect};
pub use session::{Groups, Intent, Keep, Row, min_size};

mod draw;

pub use draw::draw;

/// What only the terminal keeps of the view between frames: the scroll.
#[derive(Clone, Debug, Default)]
pub struct Dupes {
    scroll: usize,
}

/// The nodes of the files the view lists ([`session::listed`]).
pub fn listed(app: &App) -> HashSet<NodeRef> {
    session::listed(&app.session)
}

/// A group whose every copy still in the tree is ticked ([`session::whole_group_ticked`]).
pub fn whole_group_ticked(app: &App) -> Option<String> {
    session::whole_group_ticked(&app.session)
}

/// True when the view is shown, and so takes the keys it knows.
pub fn active(app: &App) -> bool {
    session::active(&app.session)
}

#[cfg(test)]
pub(crate) fn found(app: &App) -> Option<&Found> {
    session::found(&app.session)
}

/// For the view's tab ([`session::groups`]).
pub fn groups(app: &App) -> Option<Groups> {
    session::groups(&app.session)
}

/// A comparison was asked for: it runs or it is done.
pub fn compared(app: &App) -> bool {
    session::compared(&app.session)
}

/// The path of the selected file, for `y` and `o`; `None` on a group's row.
pub fn selected(app: &App) -> Option<PathBuf> {
    session::selected(&app.session)
}

/// A file still in the tree.
fn live(app: &App, c: &Candidate) -> Option<NodeRef> {
    dfdu_session::dupes::live(&app.session.extras.dupes, c)
}

/// The index in `group` of the file still in the tree that the keep rule keeps.
fn kept(app: &App, group: &Group) -> Option<usize> {
    dfdu_session::dupes::kept(group, app.session.dupes.keep, &app.session.extras.dupes)
}

fn open(app: &App, g: usize, group: &Group) -> bool {
    session::open(&app.session, g, group)
}

fn rows(app: &App) -> Vec<Row> {
    session::rows(&app.session)
}

/// Keys the view takes before the action registry; `None` passes the key on. `Enter` before
/// the first comparison starts one; the registry's moves, `Enter`/`→`, `←` and `Space` become
/// the session's intents.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    if app.session.dupes.run.is_none() && k.code == KeyCode::Enter {
        return Some(act(app, Intent::Compare));
    }
    let last = rows(app).len().saturating_sub(1);
    let page = app.session.page_rows();
    let action = keys::action_for(k)?;
    let intent = match keys::cursor_after(action, app.session.dupes.cursor, last, page) {
        Some(c) => Intent::Select(c),
        None => match action {
            Action::ToggleMark => Intent::Toggle,
            Action::Open | Action::Enter => Intent::Activate,
            Action::Parent => Intent::CloseUp,
            _ => return None,
        },
    };
    Some(act(app, intent))
}

/// Carries out `intent` ([`session::act`]) and what it leaves to the TUI.
pub fn act(app: &mut App, intent: Intent) -> Vec<Effect> {
    let then = session::act(&mut app.session, intent);
    views::carry(app, then)
}

#[cfg(test)]
mod tests;
