//! Squarified treemap layout (Bruls, Huizing and van Wijk, 2000), in a layout space whose units
//! the front end chooses ([`Area`]), and the Map view's settings and the levels below its
//! top-level tiles (`levels`).

mod levels;

pub use levels::{
    ColorBy, Info, Key, Kid, Kids, MAX_DEPTH, MAX_TILES, MIN_DEPTH, MapState, color_shown,
    cycle_color, deeper, key_of, on, read, stale,
};

/// A rectangle in layout space, in units the front end chooses: the TUI's are cell columns
/// across and cell rows times its cell aspect down, so squares there look square on screen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// `size` as a layout weight.
#[expect(
    clippy::cast_precision_loss,
    reason = "a tile's share of the screen needs far fewer than the 52 bits f64 keeps"
)]
fn weight(size: u64) -> f64 {
    size as f64
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
#[must_use]
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
    let total: f64 = sizes.iter().map(|&s| weight(s)).sum();
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
        let first = weight(sizes[start]) * scale;
        let (mut end, mut sum) = (start + 1, first);
        let mut ratio = worst(sum, first, first, side);
        while end < n {
            let a = weight(sizes[end]) * scale;
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
        let row_total: f64 = sizes[start..end].iter().map(|&s| weight(s)).sum();
        if free.w >= free.h {
            // A column along the left side.
            let w = if end == n { free.w } else { sum / free.h };
            let mut y = free.y;
            for (i, &s) in sizes[start..end].iter().enumerate() {
                let h = if start + i + 1 == end {
                    free.y + free.h - y
                } else {
                    free.h * weight(s) / row_total
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
                    free.w * weight(s) / row_total
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

    /// An area of `w` by `h` at the origin.
    fn area(w: f64, h: f64) -> Area {
        Area {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    #[test]
    fn areas_follow_sizes() {
        let area = area(120.0, 80.0);
        let sizes = [500, 300, 150, 50];
        let out = squarify(&sizes, area);
        let total = area.w * area.h;
        for (a, &s) in out.iter().zip(&sizes) {
            let want = total * weight(s) / 1000.0;
            assert!((a.w * a.h - want).abs() < 1e-6 * total, "{a:?} for {s}");
        }
    }

    #[test]
    fn ten_thousand_children_lay_out_quickly() {
        let sizes: Vec<u64> = (1..=10_000u64).rev().map(|i| i * 37 % 9973 + i).collect();
        let mut sorted = sizes;
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let area = area(200.0, 120.0);
        let t = std::time::Instant::now();
        let out = squarify(&sorted, area);
        let took = t.elapsed();
        assert_eq!(out.len(), 10_000);
        // The acceptance bound is 2 ms in a release build; a debug build gets more room.
        let bound = if cfg!(debug_assertions) { 40 } else { 2 };
        assert!(took.as_millis() < bound, "{took:?}");
    }
}
