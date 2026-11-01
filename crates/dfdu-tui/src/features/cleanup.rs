//! The cleanup view (`3`): every match of the rules in the whole scan, one of the review views
//! (`features::review`). Sections by safety level (the decision being made), each saying what
//! the level means, hold the categories found at that level, which open to their items (size,
//! entries, age, path, and a one-word reason why they are suggested or not); levels without
//! matches are left out. The selected item's rule, what deleting it loses and the reason in full
//! are in the detail pane. Ticks are the list's marks: `d` and `D` delete what is ticked through
//! the same dialog, and `Enter` on an item shows it in Files.
//! The first time the user opens the view and it has items, it ticks the suggested ones:
//! `safe` items above their rule's minimum size, `regenerable` ones only when stale, never
//! `review` or worse, and nothing changed in the last 7 days or in use. A read-only browser
//! marks nothing, as nothing marked could be deleted. `N` opens an item's native command: its dry
//! run first, then `y` runs it; `v` picks a further-reaching form, which `y` must confirm
//! twice. `A` marks the recommended items again and `f` shows only the safer levels; like `N`,
//! they are registry actions (`app::keys`) that act only while the view is shown.
//!
//! The rows, the ticks, what they mark and the native pane are the session's
//! (`dfdu_session::cleanup`): the keys turn into its intents ([`Intent`]). The scroll stays
//! here.

use dfdu_engine::Verdict;
use dfdu_rules::CategoryIx;
use dfdu_session::cleanup as session;
use dfdu_tree::Safety;
use ratatui::crossterm::event::KeyEvent;

use crate::app::keys::{self, Action};
use crate::app::views;
use crate::app::{App, Effect};
pub use session::{Intent, Item, Row, markable};

mod draw;
mod pane;

pub use draw::draw;

/// The rows the page keys move the view's cursor by.
const PAGE: usize = 10;

/// What only the terminal keeps of the view between frames: the scroll.
#[derive(Clone, Debug, Default)]
pub struct Cleanup {
    scroll: usize,
}

/// True when the view is shown, and so takes the keys it knows.
pub fn active(app: &App) -> bool {
    session::active(&app.session)
}

/// What the matches that can be ticked add up to ([`session::reclaimable`]).
pub fn reclaimable(app: &App) -> Option<u64> {
    session::reclaimable(&app.session)
}

pub(crate) fn rows(app: &App) -> Vec<Row> {
    session::rows(&app.session)
}

pub(crate) fn selected(app: &App) -> Option<&Item> {
    session::selected(&app.session)
}

/// What `Enter` does on the selected row, for the footer.
pub fn enter_does(app: &App) -> &'static str {
    session::enter_does(&app.session)
}

fn is_open(app: &App, row: Row) -> bool {
    session::is_open(&app.session, row)
}

fn is_marked(app: &App, it: &Item) -> bool {
    session::is_marked(&app.session, it)
}

fn recommended(app: &App, it: &Item, now: i64) -> Option<Verdict> {
    session::recommended(&app.session, it, now)
}

fn group_items(app: &App, cat: CategoryIx, safety: Safety) -> Vec<usize> {
    session::group_items(&app.session, cat, safety)
}

fn section_items(app: &App, safety: Safety) -> Vec<usize> {
    session::section_items(&app.session, safety)
}

/// Keys the view takes before the action registry; `None` passes the key on. The native pane
/// takes every key while it is open; otherwise the registry's moves, `Enter`/`→`, `←` and
/// `Space` become the session's intents.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    if app.session.cleanup.pane.is_some() {
        return Some(pane::key(app, k));
    }
    // What the first opening ticked is said until the next key.
    app.session.status = None;
    let last = rows(app).len().saturating_sub(1);
    let action = keys::action_for(k)?;
    let intent = match keys::cursor_after(action, app.session.cleanup.cursor, last, PAGE) {
        Some(c) => Intent::Select(c),
        None => match action {
            Action::Open | Action::Enter => Intent::Activate,
            Action::Parent => Intent::CloseUp,
            Action::ToggleMark => Intent::Toggle,
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

impl Cleanup {
    /// Keeps `cursor` on one of `rows` rows and inside the `list_h` rows shown.
    fn clamp_scroll(&mut self, cursor: &mut usize, rows: usize, list_h: usize) {
        *cursor = (*cursor).min(rows.saturating_sub(1));
        if *cursor < self.scroll {
            self.scroll = *cursor;
        } else if list_h > 0 && *cursor >= self.scroll + list_h {
            self.scroll = *cursor + 1 - list_h;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
