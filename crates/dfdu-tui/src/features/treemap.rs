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
pub use dfdu_session::treemap::{ColorBy, Key, Kids, MAX_TILES};
use place::Layout;

pub use draw::draw;

/// Below this the list is drawn instead.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 15;
/// A tile needs this many cells (6 by 3) to stay on its own.
const MIN_CELLS: f64 = 18.0;
/// A second click on the same tile within this long zooms into it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What only the terminal shows of the view: whether it had room, the tiles laid out in cells,
/// and the last click.
#[derive(Clone, Debug, Default)]
pub struct Treemap {
    /// The last frame drew the treemap (the view is on and the terminal is large enough).
    pub shown: bool,
    layout: Option<Layout>,
    last_click: Option<(Instant, NodeRef)>,
}

/// The view is the treemap (otherwise the list), also while the terminal is too small for it.
pub fn on(app: &App) -> bool {
    dfdu_session::treemap::on(&app.session)
}

/// What a read of the levels below is for ([`dfdu_session::treemap::key_of`]).
fn key_of(app: &App) -> Key {
    dfdu_session::treemap::key_of(&app.session)
}

/// The colouring drawn ([`dfdu_session::treemap::color_shown`]).
fn color_shown(app: &App) -> ColorBy {
    dfdu_session::treemap::color_shown(&app.session)
}

/// True when keys and the mouse act on the treemap: it is on and was drawn last frame.
pub fn active(app: &App) -> bool {
    on(app) && app.treemap.shown
}

/// The view was opened ([`views::switch`]): a click before it does not count towards a double
/// click.
pub fn on_open(app: &mut App) {
    app.treemap.last_click = None;
}

/// The top-level tile of the selection, as the last frame laid it out.
fn selected_top(app: &App) -> Option<usize> {
    let l = app.treemap.layout.as_ref()?;
    let node = app.session.selected.as_ref()?.node?;
    l.tops
        .iter()
        .position(|&t| l.tiles[t].entry.is_some_and(|(n, _)| n == node))
}

/// Selects the row of top-level tile `top`.
fn select_top(app: &mut App, top: usize) {
    let Some(l) = &app.treemap.layout else {
        return;
    };
    let Some((node, _)) = l.tops.get(top).and_then(|&t| l.tiles[t].entry) else {
        return;
    };
    let entries = &app.session.snap.entries;
    if let Some(row) = app
        .session
        .list
        .rows
        .iter()
        .position(|&i| entries[i].node == node)
    {
        app.session.move_to(row);
    }
}

/// Centre of `r` in layout space, where rows count [`layout::CELL_ASPECT`] times a column.
fn centre(r: Rect) -> (f64, f64) {
    (
        f64::from(r.x) + f64::from(r.width) / 2.0,
        (f64::from(r.y) + f64::from(r.height) / 2.0) * layout::CELL_ASPECT,
    )
}

/// The top-level tile nearest to `from` whose centre lies in direction `(dx, dy)`.
fn neighbour(l: &Layout, from: usize, (dx, dy): (f64, f64)) -> Option<usize> {
    let (cx, cy) = centre(l.tiles[l.tops[from]].rect);
    l.tops
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != from)
        .filter_map(|(i, &t)| {
            let (x, y) = centre(l.tiles[t].rect);
            let (ox, oy) = (x - cx, y - cy);
            (ox * dx + oy * dy > 0.0).then_some((i, ox * ox + oy * oy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Keys the treemap takes before the action registry: arrows and `hjkl` move between tiles.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let dir = match k.code {
        KeyCode::Left | KeyCode::Char('h') => (-1.0, 0.0),
        KeyCode::Right | KeyCode::Char('l') => (1.0, 0.0),
        KeyCode::Up | KeyCode::Char('k') => (0.0, -1.0),
        KeyCode::Down | KeyCode::Char('j') => (0.0, 1.0),
        _ => return None,
    };
    app.session.status = None;
    let next = app
        .treemap
        .layout
        .as_ref()
        .and_then(|l| match selected_top(app) {
            Some(from) => neighbour(l, from, dir),
            None => (!l.tops.is_empty()).then_some(0),
        });
    if let Some(top) = next {
        select_top(app, top);
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
    let tile = l.tiles.iter().rev().find(|t| t.rect.contains(at))?;
    let (node, _) = l.tiles[*l.tops.get(tile.top)?].entry?;
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
    select_top(app, top);
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
