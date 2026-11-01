//! Squarified treemap layout (`dfdu_session::treemap`), corrected for terminal cells.
//!
//! A cell is about twice as tall as it is wide, so the layout runs in a space where one row is
//! [`CELL_ASPECT`] units tall and aims for squares there; [`cells`] then rounds every edge to the
//! cell grid. Rounding edges rather than sizes keeps neighbouring tiles flush, with no gaps or
//! overlaps.

pub use dfdu_session::treemap::{Area, squarify};
use ratatui::layout::Rect;

/// Height of a terminal cell in units of its width (font dependent; 2 is typical).
pub const CELL_ASPECT: f64 = 2.0;

/// Layout space for terminal cells: cell columns across, cell rows times [`CELL_ASPECT`] down.
pub trait CellArea {
    /// The area the cells of `r` cover.
    fn of(r: Rect) -> Self;
}

impl CellArea for Area {
    fn of(r: Rect) -> Self {
        Area {
            x: f64::from(r.x),
            y: f64::from(r.y) * CELL_ASPECT,
            w: f64::from(r.width),
            h: f64::from(r.height) * CELL_ASPECT,
        }
    }
}

/// The cells `a` covers: each edge rounded to the nearest grid line.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the edges lie within the u16 Rect that was laid out, and `as` saturates past it"
)]
pub fn cells(a: Area) -> Rect {
    let x0 = a.x.round();
    let x1 = (a.x + a.w).round();
    let y0 = (a.y / CELL_ASPECT).round();
    let y1 = ((a.y + a.h) / CELL_ASPECT).round();
    Rect::new(
        x0 as u16,
        y0 as u16,
        (x1 - x0).max(0.0) as u16,
        (y1 - y0).max(0.0) as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(rects: &[Rect], area: Rect) -> Vec<u8> {
        let mut grid = vec![0u8; area.width as usize * area.height as usize];
        for r in rects {
            for y in r.y..r.y + r.height {
                for x in r.x..r.x + r.width {
                    assert!(area.contains((x, y).into()), "{r:?} leaves {area:?}");
                    grid[(y - area.y) as usize * area.width as usize + (x - area.x) as usize] += 1;
                }
            }
        }
        grid
    }

    #[test]
    fn tiles_cover_the_area_once() {
        let area = Rect::new(3, 2, 97, 41);
        for sizes in [
            vec![100],
            vec![60, 30, 10],
            vec![6, 6, 4, 3, 2, 2, 1],
            (1..=200).rev().map(|i| i * i).collect::<Vec<u64>>(),
        ] {
            let tiles: Vec<Rect> = squarify(&sizes, Area::of(area))
                .into_iter()
                .map(cells)
                .collect();
            assert!(covered(&tiles, area).iter().all(|&n| n == 1), "{sizes:?}");
        }
    }

    #[test]
    fn tiles_are_square_on_screen_not_in_cells() {
        // Four equal tiles in a terminal 80 columns by 20 rows (80 x 40 units): squares in
        // layout space are 20 columns by 10 rows, not 10 by 10.
        let out = squarify(&[1, 1, 1, 1], Area::of(Rect::new(0, 0, 80, 20)));
        for a in out {
            let r = cells(a);
            assert_eq!((r.width, r.height), (40, 10), "{r:?}");
        }
    }

    #[test]
    fn zero_sizes_get_nothing() {
        let out = squarify(&[10, 0, 0], Area::of(Rect::new(0, 0, 10, 10)));
        assert_eq!(cells(out[0]), Rect::new(0, 0, 10, 10));
        assert!(out[1..].iter().all(|a| a.w * a.h == 0.0));
        assert!(
            squarify(&[0, 0], Area::of(Rect::new(0, 0, 5, 5)))
                .iter()
                .all(|a| a.w == 0.0)
        );
    }
}
