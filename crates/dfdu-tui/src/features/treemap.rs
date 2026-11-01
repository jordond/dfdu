//! The Map view (`2`): the current directory as squarified tiles.
//!
//! The tiles at the top level are the directory's rows, so the selection, marks, delete, details
//! and filter are the list's. At depth 1 (the default) only the selected tile shows what is in
//! it: what `Enter` opens. `+` shows up to [`MAX_DEPTH`] levels in every tile. The levels below
//! the top, the depth and the colouring are the session's (`dfdu_session::treemap`). The layout
//! is recomputed when the list re-sorts (`List::sort_gen`), when the
//! area or the depth changes, when the tree's matches change (a scan classifies as it goes),
//! and otherwise kept, so tiles do not jump while sizes stream in; the labels of top-level
//! tiles show live sizes.
//!
//! Tiles too small to stay on their own are merged into one tile per parent. Below 60x15 cells
//! the list is drawn instead.

mod draw;
mod fill;
mod glyph;
pub(crate) mod layout;
mod paint;
mod place;
mod tile;

use std::time::{Duration, Instant};

use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Position, Rect};

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
pub use dfdu_session::treemap::ColorBy;
use dfdu_session::treemap::Toward;
use place::Layout;

pub use draw::draw;

/// Below this the list is drawn instead.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 15;
/// A tile needs this many cells (6 by 3) to stay on its own.
const MIN_CELLS: f64 = 18.0;
/// A second click on the same tile within this long zooms into it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What only the terminal shows of the view: the tiles laid out in cells, and the last click.
/// Whether the last frame had room for it is the session's viewport
/// ([`SessionState::map_drawn`](dfdu_session::SessionState::map_drawn)).
#[derive(Clone, Debug, Default)]
pub struct Treemap {
    layout: Option<Layout>,
    last_click: Option<(Instant, NodeRef)>,
}

/// The colouring drawn ([`dfdu_session::treemap::color_shown`]).
fn color_shown(app: &App) -> ColorBy {
    dfdu_session::treemap::color_shown(&app.session)
}

/// True when keys and the mouse act on the treemap: it is on and was drawn last frame.
pub fn active(app: &App) -> bool {
    app.session.map_drawn()
}

/// The view was opened ([`views::switch`]): a click before it does not count towards a double
/// click.
pub fn on_open(app: &mut App) {
    app.treemap.last_click = None;
}

/// The top-level tile of the selection, as the last frame laid it out.
fn selected_top(app: &App) -> Option<usize> {
    (app.treemap.layout.as_ref()?.map).selected_top(&app.session)
}

/// Keys the treemap takes before the action registry: arrows and `hjkl` move between tiles.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let toward = match k.code {
        KeyCode::Left | KeyCode::Char('h') => Toward::Left,
        KeyCode::Right | KeyCode::Char('l') => Toward::Right,
        KeyCode::Up | KeyCode::Char('k') => Toward::Up,
        KeyCode::Down | KeyCode::Char('j') => Toward::Down,
        _ => return None,
    };
    match &app.treemap.layout {
        Some(l) => dfdu_session::treemap::step(&mut app.session, &l.map, toward),
        None => app.session.status = None,
    }
    Some(Vec::new())
}

/// A click on the treemap: selects the top-level tile under it; a second click zooms in.
/// `None` when the click is outside the tiles.
pub fn click(app: &mut App, at: Position, now: Instant) -> Option<Vec<Effect>> {
    let l = app.treemap.layout.as_ref()?;
    if !l.area.contains(at) {
        return None;
    }
    let m = &l.map;
    let tile = &m.tiles[m.at(f64::from(at.x), f64::from(at.y))?];
    let (node, _) = m.tiles[*m.tops.get(tile.top)?].entry?;
    // A "+N small" tile at the top level selects nothing.
    if tile.level == 0 && tile.entry.is_none() {
        return Some(Vec::new());
    }
    let top = tile.top;
    let again = app
        .treemap
        .last_click
        .is_some_and(|(t, n)| n == node && now.duration_since(t) <= DOUBLE_CLICK);
    app.session.status = None;
    dfdu_session::treemap::select_top(&mut app.session, m, top);
    if again {
        app.treemap.last_click = None;
        return Some(keys::run(app, Action::Enter));
    }
    app.treemap.last_click = Some((now, node));
    Some(Vec::new())
}

/// Whether the treemap fits a terminal of size `area`; the list is drawn otherwise.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

#[cfg(test)]
pub(crate) mod tests;
