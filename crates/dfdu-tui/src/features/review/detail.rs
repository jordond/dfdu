//! The detail pane of a review view: on the right of the rows from [`DETAIL_RIGHT_FROM`]
//! columns, below them otherwise ([`split`]), with a rule between them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::theme::Theme;

/// From this many columns the detail pane goes on the right of the rows; below, under them.
const DETAIL_RIGHT_FROM: u16 = 110;
/// The detail pane on the right takes this share of the width, within the bounds below.
const DETAIL_SHARE: u16 = 38;
const DETAIL_MIN_WIDTH: u16 = 36;
const DETAIL_MAX_WIDTH: u16 = 60;
/// Below the rows, the detail pane takes at most this many lines, and only when it leaves the
/// rows this many.
const DETAIL_MAX_HEIGHT: u16 = 6;
const ROWS_MIN: u16 = 8;

/// Where the rows and the detail pane go in `area`: the pane on the right from
/// [`DETAIL_RIGHT_FROM`] columns, else below the rows with at most `wants` lines. `None` for the
/// pane when there is no room for it.
pub fn split(area: Rect, wants: u16) -> (Rect, Option<Rect>) {
    if area.width >= DETAIL_RIGHT_FROM {
        let w = (area.width * DETAIL_SHARE / 100).clamp(DETAIL_MIN_WIDTH, DETAIL_MAX_WIDTH);
        let rows = Rect {
            width: area.width - w,
            ..area
        };
        let pane = Rect {
            x: area.x + area.width - w,
            width: w,
            ..area
        };
        return (rows, Some(pane));
    }
    let h = (wants + 1)
        .min(DETAIL_MAX_HEIGHT + 1)
        .min(area.height.saturating_sub(ROWS_MIN));
    // The rule above it, and two lines at least.
    if h < 3 {
        return (area, None);
    }
    let rows = Rect {
        height: area.height - h,
        ..area
    };
    let pane = Rect {
        y: area.y + area.height - h,
        height: h,
        ..area
    };
    (rows, Some(pane))
}

/// Draws `lines` in the detail pane `area` from [`split`], wrapped and a cell in from each
/// side: a rule on its left side when it is on the right of the rows, above it when it is
/// below them.
pub fn draw_detail(f: &mut Frame, t: &Theme, area: Rect, right: bool, lines: Vec<Line<'_>>) {
    let block = Block::new()
        .borders(if right { Borders::LEFT } else { Borders::TOP })
        .border_style(t.border)
        .padding(Padding::horizontal(1));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Whether the detail pane in `area` from [`split`] is on the right of the rows.
pub fn on_right(area: Rect) -> bool {
    area.width >= DETAIL_RIGHT_FROM
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detail_pane_goes_right_when_wide_and_below_when_narrow() {
        let (rows, pane) = split(Rect::new(0, 0, 120, 20), 4);
        let pane = pane.expect("a pane");
        assert_eq!(rows.width + pane.width, 120);
        assert_eq!(pane.height, 20);
        let (rows, pane) = split(Rect::new(0, 0, 80, 20), 4);
        let pane = pane.expect("a pane");
        assert_eq!((rows.height, pane.height), (15, 5));
        // Too short for both: the rows only.
        assert!(split(Rect::new(0, 0, 80, 10), 4).1.is_none());
    }
}
