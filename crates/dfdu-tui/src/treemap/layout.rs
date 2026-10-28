//! Squarified treemap layout (Bruls, Huizing and van Wijk, 2000), corrected for terminal cells.
//!
//! A cell is about twice as tall as it is wide, so the layout runs in a space where one row is
//! [`CELL_ASPECT`] units tall and aims for squares there; [`cells`] then rounds every edge to the
//! cell grid. Rounding edges rather than sizes keeps neighbouring tiles flush, with no gaps or
//! overlaps.

use ratatui::layout::Rect;

/// Height of a terminal cell in units of its width (font dependent; 2 is typical).
pub const CELL_ASPECT: f64 = 2.0;

/// A rectangle in layout space: cell columns across, cell rows times [`CELL_ASPECT`] down.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Area {
    pub fn of(r: Rect) -> Self {
        Area {
            x: r.x as f64,
            y: r.y as f64 * CELL_ASPECT,
            w: r.width as f64,
            h: r.height as f64 * CELL_ASPECT,
        }
    }
}

/// The cells `a` covers: each edge rounded to the nearest grid line.
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

/// The worst aspect ratio of a row of tiles with total weight `sum`, largest `max` and smallest
/// `min`, laid along a side of length `side` (weights already scaled to areas).
fn worst(sum: f64, max: f64, min: f64, side: f64) -> f64 {
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Lays out `sizes`, largest first, in `area`: one area per size, in the same order, together
/// covering `area` exactly. Zero sizes get empty areas.
pub fn squarify(sizes: &[u64], area: Area) -> Vec<Area> {
    let mut out = vec![
        Area {
            x: area.x,
            y: area.y,
            w: 0.0,
            h: 0.0,
        };
        sizes.len()
    ];
    let total: f64 = sizes.iter().map(|&s| s as f64).sum();
    let n = sizes.iter().rposition(|&s| s > 0).map_or(0, |i| i + 1);
    if n == 0 || area.w <= 0.0 || area.h <= 0.0 {
        return out;
    }
    let scale = area.w * area.h / total;
    let mut free = area;
    let mut start = 0;
    while start < n {
        let side = free.w.min(free.h);
        // Grow the row while that does not make its worst ratio worse.
        let first = sizes[start] as f64 * scale;
        let (mut end, mut sum) = (start + 1, first);
        let mut ratio = worst(sum, first, first, side);
        while end < n {
            let a = sizes[end] as f64 * scale;
            // Sizes are sorted, so the new one is the row's smallest and the first its largest.
            let next = worst(sum + a, first, a, side);
            if next > ratio {
                break;
            }
            ratio = next;
            sum += a;
            end += 1;
        }
        // The last row takes what is left, so floating-point error never leaves a sliver.
        let row_total: f64 = sizes[start..end].iter().map(|&s| s as f64).sum();
        if free.w >= free.h {
            // A column along the left side.
            let w = if end == n { free.w } else { sum / free.h };
            let mut y = free.y;
            for (i, &s) in sizes[start..end].iter().enumerate() {
                let h = if start + i + 1 == end {
                    free.y + free.h - y
                } else {
                    free.h * s as f64 / row_total
                };
                out[start + i] = Area { x: free.x, y, w, h };
                y += h;
            }
            free.x += w;
            free.w -= w;
        } else {
            // A row along the top.
            let h = if end == n { free.h } else { sum / free.w };
            let mut x = free.x;
            for (i, &s) in sizes[start..end].iter().enumerate() {
                let w = if start + i + 1 == end {
                    free.x + free.w - x
                } else {
                    free.w * s as f64 / row_total
                };
                out[start + i] = Area { x, y: free.y, w, h };
                x += w;
            }
            free.y += h;
            free.h -= h;
        }
        start = end;
    }
    out
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
    fn areas_follow_sizes() {
        let area = Area::of(Rect::new(0, 0, 120, 40));
        let sizes = [500, 300, 150, 50];
        let out = squarify(&sizes, area);
        let total = area.w * area.h;
        for (a, &s) in out.iter().zip(&sizes) {
            let want = total * s as f64 / 1000.0;
            assert!((a.w * a.h - want).abs() < 1e-6 * total, "{a:?} for {s}");
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

    #[test]
    fn ten_thousand_children_lay_out_quickly() {
        let sizes: Vec<u64> = (1..=10_000u64).rev().map(|i| i * 37 % 9973 + i).collect();
        let mut sorted = sizes;
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let area = Area::of(Rect::new(0, 0, 200, 60));
        let t = std::time::Instant::now();
        let out = squarify(&sorted, area);
        let took = t.elapsed();
        assert_eq!(out.len(), 10_000);
        // The acceptance bound is 2 ms in a release build; a debug build gets more room.
        let bound = if cfg!(debug_assertions) { 40 } else { 2 };
        assert!(took.as_millis() < bound, "{took:?}");
    }
}
