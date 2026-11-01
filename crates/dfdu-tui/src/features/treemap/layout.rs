//! Terminal cells as the units of the session's tile tree (`dfdu_session::treemap`).
//!
//! A cell is about twice as tall as it is wide, so the layout runs in a space where one row is
//! [`CELL_ASPECT`] units tall and aims for squares there; the tile tree's grid then rounds every
//! edge to the cell grid. Rounding edges rather than sizes keeps neighbouring tiles flush, with
//! no gaps or overlaps. A tile's rectangle is in cells, whole ones: [`rect`] reads it as a
//! `Rect`.

pub use dfdu_session::treemap::Area;
use ratatui::layout::Rect;

/// Height of a terminal cell in units of its width (font dependent; 2 is typical).
pub const CELL_ASPECT: f64 = 2.0;

/// The cells of `r` as an area of the tile tree.
pub fn area_of(r: Rect) -> Area {
    Area {
        x: f64::from(r.x),
        y: f64::from(r.y),
        w: f64::from(r.width),
        h: f64::from(r.height),
    }
}

/// The cells `a` covers, its edges on whole cells as the cell grid snaps them.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the edges lie within the u16 Rect that was laid out, and `as` saturates past it"
)]
pub fn rect(a: Area) -> Rect {
    Rect::new(a.x as u16, a.y as u16, a.w as u16, a.h as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip() {
        let r = Rect::new(3, 2, 97, 41);
        assert_eq!(rect(area_of(r)), r);
        assert_eq!(area_of(r).h, 41.0, "in cells, not layout space");
    }
}
