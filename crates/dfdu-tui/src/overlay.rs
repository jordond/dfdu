//! The overlays, drawn over the browser and taking every key while open: `help`, `error_list`,
//! `confirm` (the delete confirmation), `palette`, `type_filter` (the file type picker) and
//! `mark_pattern` (the glob that marks names).
//! Also the sizing and cursor keys the searchable lists among them share.

pub(crate) mod confirm;
pub(crate) mod error_list;
pub(crate) mod help;
pub(crate) mod mark_pattern;
pub(crate) mod palette;
pub(crate) mod type_filter;

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::App;
use crate::view::cells;

/// Rows a searchable list overlay spends on its border and search line.
const LIST_CHROME: u16 = 3;

/// The width of an overlay centred in `area`: at most `max`, leaving two columns on each side,
/// but at least 24 columns when the area has them.
pub fn width(area: Rect, max: u16) -> u16 {
    area.width
        .saturating_sub(4)
        .min(max)
        .max(area.width.min(24))
}

/// The height of an overlay centred in `area` showing `rows` lines inside `chrome` rows of
/// border and prompt: leaving a row above and below, but at least 4 rows when the area has them.
pub fn height(area: Rect, rows: usize, chrome: u16) -> u16 {
    cells(rows)
        .saturating_add(chrome)
        .min(area.height.saturating_sub(2))
        .max(area.height.min(4))
}

/// The height of a searchable list overlay in `area` with `rows` rows (one when empty, for the
/// "nothing matches" line).
pub fn list_height(area: Rect, rows: usize) -> u16 {
    height(area, rows.max(1), LIST_CHROME)
}

/// Draws `lines` in a bordered box titled `title`, `width` wide and as high as the lines,
/// centred in `area`, and returns the rectangle it covers.
pub fn dialog<'a>(
    f: &mut Frame,
    area: Rect,
    width: u16,
    title: impl Into<Line<'a>>,
    lines: Vec<Line<'a>>,
) -> Rect {
    let r = area.centered(
        Constraint::Length(width),
        Constraint::Length(cells(lines.len()).saturating_add(2)),
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        r,
    );
    r
}

/// The rows the open list overlay showed in the last frame: a page for `PgUp` and `PgDn`.
pub fn list_page(app: &App) -> usize {
    app.areas
        .overlay
        .map_or(1, |r| usize::from(r.height.saturating_sub(LIST_CHROME)))
        .max(1)
}

/// Where the cursor of a list overlay goes for `code`, with `last` the index of its last row;
/// `None` for keys that do not move it.
pub fn list_cursor(code: KeyCode, cursor: usize, last: usize, page: usize) -> Option<usize> {
    match code {
        KeyCode::Up => Some(cursor.saturating_sub(1)),
        KeyCode::Down => Some((cursor + 1).min(last)),
        KeyCode::PageUp => Some(cursor.saturating_sub(page)),
        KeyCode::PageDown => Some((cursor + page).min(last)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_cursor_stays_on_the_rows() {
        assert_eq!(list_cursor(KeyCode::Up, 0, 5, 3), Some(0));
        assert_eq!(list_cursor(KeyCode::Down, 5, 5, 3), Some(5));
        assert_eq!(list_cursor(KeyCode::Down, 2, 5, 3), Some(3));
        assert_eq!(list_cursor(KeyCode::PageDown, 4, 5, 3), Some(5));
        assert_eq!(list_cursor(KeyCode::PageUp, 4, 5, 3), Some(1));
        assert_eq!(list_cursor(KeyCode::Enter, 4, 5, 3), None);
    }

    #[test]
    fn overlays_fit_the_area() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(width(area, 76), 76);
        assert_eq!(width(Rect::new(0, 0, 20, 30), 76), 20);
        assert_eq!(list_height(area, 0), 4);
        assert_eq!(list_height(area, 10), 13);
        assert_eq!(list_height(area, 100), 28);
        assert_eq!(height(Rect::new(0, 0, 100, 3), 10, 2), 3);
    }
}
