//! The tiles in cells: the session's tile tree (`dfdu_session::treemap`) laid out on the cell
//! grid ([`grid`]), with each tile's rectangle snapped to whole cells ([`rect`] reads it as
//! one). At depth 1 only the selected tile shows what is in it, laid out on its own by
//! [`peek`].
//!
//! A tile that shows its children keeps a header row for its name, and in colour a margin of
//! one cell on its other sides, in which the selection's edge is drawn; without colour it keeps
//! the frame and a line under it.

use dfdu_session::treemap::{self as map, Grid, Inset, TileTree};
use ratatui::layout::Rect;

use super::MIN_CELLS;
use super::layout::{CELL_ASPECT, area_of};
use super::tile::LABEL_MIN;
use crate::app::App;

pub(super) use super::layout::rect;
pub(super) use dfdu_session::treemap::Tile;

/// The smallest tile that shows its children.
const NEST_MIN: (f64, f64) = (8.0, 5.0);
/// The smallest tile drawn with a frame without colour.
const FRAME_MIN: (f64, f64) = (4.0, 3.0);

/// The tiles as the last frame laid them out, and where.
#[derive(Clone, Debug)]
pub(super) struct Layout {
    pub map: TileTree,
    pub area: Rect,
}

/// The cell grid: cells twice as tall as wide, edges on whole cells, and below a tile's header
/// row its children framed (`framed`) or with a cell of margin.
pub(super) fn grid(framed: bool) -> Grid {
    let label = |(w, h): (u16, u16)| (f64::from(w), f64::from(h));
    Grid {
        aspect: CELL_ASPECT,
        snap: true,
        min_area: MIN_CELLS,
        nest_min: NEST_MIN,
        label_min: label(LABEL_MIN),
        frame_min: FRAME_MIN,
        inset: if framed {
            Inset {
                left: 1.0,
                top: 2.0,
                right: 1.0,
                bottom: 1.0,
            }
        } else {
            Inset {
                left: 1.0,
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
            }
        },
    }
}

impl Layout {
    /// It still shows the view as it stands, in `area`, `framed` or not.
    pub(super) fn current(&self, app: &App, area: Rect, framed: bool) -> bool {
        self.area == area && (self.map).current(&app.session, area_of(area), &grid(framed))
    }
}

/// Lays out the rows of the directory in `area`, and below them the levels the depth shows
/// (none at depth 1, where [`peek`] shows the selected tile's).
pub(super) fn build(app: &App, area: Rect, framed: bool) -> Layout {
    Layout {
        map: map::build(&app.session, area_of(area), &grid(framed)),
        area,
    }
}

/// At depth 1, the children of top-level tile `at` of `l` (the selected one), laid out inside
/// it, the tile itself marked as showing them; nothing when the tile is too small or nothing
/// was read below it.
pub(super) fn peek(app: &App, l: &Layout, at: usize) -> Vec<Tile> {
    map::peek(&app.session, &l.map, at)
}

/// A top-level file tile named `name` of `size` bytes filling `r`, alone in it.
#[cfg(test)]
pub(super) fn tile_of(r: Rect, name: &str, size: u64) -> Tile {
    let a = area_of(r);
    Tile {
        rect: a,
        bound: a,
        level: 0,
        top: 0,
        up: None,
        entry: None,
        node: None,
        name: name.into(),
        is_dir: false,
        size,
        parent_size: size,
        mtime: 0,
        tag: None,
        dominant: None,
        small: 0,
        nested: false,
        room: map::Room::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::features::treemap::tests::{map_of, nested_app, treemap_app};
    use crate::model::snapshot::test_entry;
    use crate::tests::tree;

    const AREA: Rect = Rect::new(0, 2, 80, 20);

    #[test]
    fn rows_become_tiles_inside_the_area_largest_first() {
        let fx = tree();
        let app = treemap_app(&fx.m, Instant::now());
        let l = build(&app, AREA, false);
        let m = &l.map;
        let names: Vec<&str> = m.tops.iter().map(|&t| m.tiles[t].name.as_str()).collect();
        assert_eq!(names, ["big", "mid", "small", ".hid"]);
        for t in &m.tiles {
            assert!(AREA.contains(rect(t.rect).as_position()), "{t:?}");
            assert_eq!(rect(t.bound), AREA);
            assert_eq!(t.level, 0, "depth 1 lays out the top level only");
        }
        let cells: u32 = m.tiles.iter().map(|t| rect(t.rect).area()).sum();
        assert!(cells <= AREA.area());
        // The rest of the rows is one merged tile.
        let merged: Vec<&Tile> = m.tiles.iter().filter(|t| t.small > 0).collect();
        assert!(merged.len() <= 1, "{merged:?}");
        assert!(l.current(&app, AREA, false));
        assert!(!l.current(&app, AREA, true), "framed is laid out again");
    }

    #[test]
    fn tiny_rows_merge_into_one_tile_and_the_largest_keeps_its_own() {
        let t0 = Instant::now();
        let mut entries = vec![test_entry(0, "huge", 1_000_000)];
        entries.extend((1..200).map(|i| test_entry(i, &format!("t{i}"), 100)));
        let app = map_of(entries, t0);
        let m = build(&app, AREA, false).map;
        let merged: Vec<&Tile> = m.tiles.iter().filter(|t| t.small > 0).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!((merged[0].small, merged[0].size), (199, 19_900));
        assert_eq!(m.tiles[m.tops[0]].name, "huge");
        assert_eq!(m.tops.len(), 1, "the merged tile is not one of the rows");
    }

    #[test]
    fn deeper_levels_nest_inside_their_tile() {
        let fx = tree();
        let t0 = Instant::now();
        let app = nested_app(&fx.m, t0);
        let m = build(&app, AREA, true).map;
        let big = m.tops[0];
        assert!(m.tiles[big].nested, "big shows what is in it");
        let inside: Vec<&Tile> = m.tiles.iter().filter(|t| t.up == Some(big)).collect();
        assert!(!inside.is_empty());
        let frame = rect(m.tiles[big].rect);
        for t in inside {
            assert_eq!(t.level, 1);
            assert_eq!(t.top, 0);
            // Framed: inside the frame and below the label line.
            let r = rect(t.rect);
            assert!(r.y >= frame.y + 2 && r.x > frame.x, "{t:?}");
            assert_eq!(t.parent_size, m.tiles[big].size);
        }
    }

    #[test]
    fn peek_lays_out_the_selected_tile_alone() {
        let fx = tree();
        let app = treemap_app(&fx.m, Instant::now());
        let l = build(&app, AREA, false);
        let big = l.map.tops[0];
        let tiles = peek(&app, &l, big);
        assert!(tiles[0].nested && tiles[0].rect == l.map.tiles[big].rect);
        let names: Vec<&str> = tiles[1..].iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(tiles[1..].iter().all(|t| t.up == Some(0) && t.level == 1));
        // A file has nothing to show.
        let small = l.map.tops[2];
        assert!(peek(&app, &l, small).is_empty());
    }
}
