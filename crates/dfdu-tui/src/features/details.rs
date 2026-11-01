//! The details pane (`i`): everything the tree knows about the selected entry. On the right at
//! 100 columns or more, as a sheet below the list when narrower. `Tab` gives it the focus, and
//! the arrow and page keys then scroll it; `Tab` again opens the marks pane in its place.
//! Whether it is open and what it reads are the session's (`dfdu_session::details`); the focus,
//! the scroll and the summary in the status line stay here.

mod read;
mod text;

use dfdu_tree::Kind;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::{App, Effect};
use crate::features::marks_pane;
use crate::format::{count, human};
use crate::view::cells;

pub use read::{Facts, Largest, Matched};
use text::lines;
#[cfg(test)]
use text::{state, wrap};

/// The pane goes on the right from this width of the area below the header...
const RIGHT_FROM: u16 = 100;
/// ...this wide, or a third of the width if that is more.
const RIGHT_WIDTH: u16 = 40;
/// Otherwise it is a sheet this high below the list...
const SHEET_HEIGHT: u16 = 10;
/// ...if the list keeps this many rows.
const LIST_MIN: u16 = 6;
const BAR: usize = 8;

/// What only the terminal keeps of the pane: the focus, the scroll and the summary.
#[derive(Clone, Debug, Default)]
pub struct Details {
    focused: bool,
    /// First line shown.
    pub(crate) scroll: usize,
    /// The summary [`split`] put in the status line in place of a pane that does not fit, to
    /// replace as the selection moves.
    summary: Option<String>,
}

/// Opens or closes the pane, which then starts without the focus, at its top.
pub fn toggle(app: &mut App) {
    dfdu_session::details::toggle(&mut app.session);
    app.details = Details::default();
}

/// True while the pane is open.
fn open(app: &App) -> bool {
    app.session.details.open
}

/// `Tab`: moves the focus from the list to this pane when it is shown, from here to the marks
/// pane, and from there back to the list. While the last frame had no room for this pane, it
/// has none for the marks pane either, and the focus stays on the list.
pub fn focus(app: &mut App) -> Vec<Effect> {
    if marks_pane::close(app) || (open(app) && app.areas.details.is_none()) {
        return Vec::new();
    }
    if open(app) && !app.details.focused {
        app.details.focused = true;
        return Vec::new();
    }
    app.details.focused = false;
    marks_pane::show(app)
}

/// True while the pane has the focus.
pub fn focused(app: &App) -> bool {
    open(app) && app.details.focused
}

/// Handles a key while the pane has the focus; `None` passes it on to the list.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let page = app
        .areas
        .details
        .map_or(1, |r| i32::from(r.height.saturating_sub(2).max(1)));
    match k.code {
        KeyCode::Up => scroll(app, -1),
        KeyCode::Down => scroll(app, 1),
        KeyCode::PageUp => scroll(app, -page),
        KeyCode::PageDown => scroll(app, page),
        KeyCode::Tab => return Some(focus(app)),
        KeyCode::Esc => {
            app.details.focused = false;
            true
        }
        KeyCode::Char('i') => {
            toggle(app);
            true
        }
        _ => false,
    }
    .then(Vec::new)
}

/// Scrolls the pane by `delta` lines, focused or not (the mouse wheel over it). False when the
/// pane is not shown, so the caller can move the list instead.
pub fn scroll(app: &mut App, delta: i32) -> bool {
    if marks_pane::is_open(app) {
        return marks_pane::scroll(app, delta);
    }
    let Some(r) = app.areas.details.filter(|_| open(app)) else {
        return false;
    };
    let inner = Block::bordered().inner(r);
    let max = lines(app, inner.width as usize)
        .len()
        .saturating_sub(inner.height as usize);
    app.details.scroll = app
        .details
        .scroll
        .saturating_add_signed(delta as isize)
        .min(max);
    true
}

/// The selected row in one line, for the status line when the pane does not fit: its name,
/// size and, for a directory, its items.
fn summary(app: &App) -> Option<String> {
    let (name, is_dir, size, items) = if crate::features::cleanup::active(app) {
        let it = crate::features::cleanup::selected(app)?;
        let name = it.path.file_name().unwrap_or(it.path.as_bytes());
        let is_dir = it.kind == Kind::Dir;
        (
            String::from_utf8_lossy(name).into_owned(),
            is_dir,
            it.size,
            it.items,
        )
    } else {
        let e = app.session.selected_entry()?;
        let name = String::from_utf8_lossy(&e.name).into_owned();
        (name, e.is_dir, e.size, e.items)
    };
    Some(if is_dir {
        format!("▸ {name}/  {} · {} items", human(size), count(items))
    } else {
        format!("▸ {name}  {}", human(size))
    })
}

/// Splits the area below the header into the list and the pane, if the pane (or the marks pane
/// in its place) is open and fits.
/// When it does not fit, the list view's selection line already sums up the selected row; the
/// other views show a summary in the status line instead, unless it has another message to show.
pub fn split(app: &mut App, area: Rect, list_view: bool) -> (Rect, Option<Rect>) {
    if !open(app) && !marks_pane::is_open(app) {
        return (area, None);
    }
    let parts = if area.width >= RIGHT_FROM {
        let w = RIGHT_WIDTH.max(area.width / 3);
        Some(Layout::horizontal([Constraint::Min(1), Constraint::Length(w)]).areas(area))
    } else if area.height >= SHEET_HEIGHT + LIST_MIN {
        Some(Layout::vertical([Constraint::Min(1), Constraint::Length(SHEET_HEIGHT)]).areas(area))
    } else {
        None
    };
    let ours = app.session.status.is_none() || app.session.status == app.details.summary;
    if let Some([list, pane]) = parts {
        if ours {
            app.session.status = None;
        }
        app.details.summary = None;
        (list, Some(pane))
    } else {
        app.details.focused = false;
        if marks_pane::close(app) {
            app.session.status =
                Some("no room for the marks pane: make the terminal larger".into());
            if !open(app) {
                return (area, None);
            }
        }
        let line = if list_view { None } else { summary(app) };
        if ours {
            app.session.status.clone_from(&line);
        }
        app.details.summary = line;
        (area, None)
    }
}

/// Draws the pane, titled with the entry's name: a rounded border, heavy while it has the focus.
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let (border, style) = if focused(app) {
        (BorderType::Thick, t.border_focus)
    } else {
        (BorderType::Rounded, t.border)
    };
    let title = match &app.session.extras.details.facts {
        Some(f) => format!(" {}{} ", f.name, if f.is_dir { "/" } else { "" }),
        None => " Details ".into(),
    };
    let mut block = Block::bordered()
        .border_type(border)
        .border_style(style)
        .title(Span::styled(title, t.title));
    let inner = block.inner(area);
    let lines = lines(app, inner.width as usize);
    let max = lines.len().saturating_sub(inner.height as usize);
    let scroll = app.details.scroll.min(max);
    if scroll < max {
        block = block.title_bottom(Line::styled(" ↓ more ", t.fg_dim).right_aligned());
    }
    f.render_widget(
        Paragraph::new(lines)
            .scroll((cells(scroll), 0))
            .block(block),
        area,
    );
}

/// True when the pane shows something other than what was last read
/// ([`dfdu_session::details::stale`]).
#[cfg(test)]
fn stale(app: &App) -> bool {
    dfdu_session::details::stale(&app.session)
}

#[cfg(test)]
mod tests;
