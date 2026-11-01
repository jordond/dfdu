use std::sync::Arc;

use super::*;
use crate::snapshot::test_entry;
use crate::state::tests::browser;
use crate::treemap::Kid;
use crate::view::View;

/// Terminal cells: twice as tall as wide, snapped, with the TUI's thresholds and a header row.
const CELLS: Grid = Grid {
    aspect: 2.0,
    snap: true,
    min_area: 18.0,
    nest_min: (8.0, 5.0),
    label_min: (8.0, 2.0),
    frame_min: (4.0, 3.0),
    inset: Inset {
        left: 1.0,
        top: 1.0,
        right: 1.0,
        bottom: 1.0,
    },
};

/// Points: square, snapped, with room for a line of text on top.
const POINTS: Grid = Grid {
    aspect: 1.0,
    snap: true,
    min_area: 400.0,
    nest_min: (60.0, 40.0),
    label_min: (40.0, 18.0),
    frame_min: (6.0, 6.0),
    inset: Inset {
        left: 2.0,
        top: 18.0,
        right: 2.0,
        bottom: 2.0,
    },
};

fn area(x: f64, y: f64, w: f64, h: f64) -> Area {
    Area { x, y, w, h }
}

/// A Map of `/r` holding `big` (a directory of `a` and `b`), `mid` and `small`.
fn map() -> SessionState {
    let mut big = test_entry(0, "big", 6000);
    big.is_dir = true;
    let mut s = browser(vec![
        big,
        test_entry(1, "mid", 3000),
        test_entry(2, "small", 1000),
    ]);
    s.view = View::Map;
    let kid = |i, name: &str, size| Kid {
        node: test_entry(i, name, size).node,
        name: name.as_bytes().into(),
        is_dir: false,
        size,
        mtime: 0,
        tag: None,
        kids: None,
    };
    let node = s.snap.entries[0].node;
    let kids = Kids {
        items: vec![kid(10, "a", 4000), kid(11, "b", 2000)],
        rest: (0, 0),
    };
    s.extras.treemap.below = Arc::new(HashMap::from([(node, kids)]));
    s
}

/// How many tiles cover each unit of `a`, a grid snapped to whole units.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "small whole numbers"
)]
fn covered(rects: &[Area], a: Area) -> Vec<u8> {
    let (w, h) = (a.w as usize, a.h as usize);
    let mut grid = vec![0u8; w * h];
    for r in rects {
        for y in r.y as usize..(r.y + r.h) as usize {
            for x in r.x as usize..(r.x + r.w) as usize {
                assert!(a.contains(x as f64, y as f64), "{r:?} leaves {a:?}");
                grid[(y - a.y as usize) * w + (x - a.x as usize)] += 1;
            }
        }
    }
    grid
}

#[test]
fn snapped_tiles_cover_the_area_once() {
    let a = area(3.0, 2.0, 97.0, 41.0);
    for sizes in [
        vec![100],
        vec![60, 30, 10],
        vec![6, 6, 4, 3, 2, 2, 1],
        (1..=200).rev().map(|i| i * i).collect::<Vec<u64>>(),
    ] {
        let tiles: Vec<Area> = (squarify(&sizes, CELLS.layout(a)).into_iter())
            .map(|l| CELLS.screen(l))
            .collect();
        assert!(covered(&tiles, a).iter().all(|&n| n == 1), "{sizes:?}");
    }
}

#[test]
fn tiles_are_square_on_screen_not_in_units() {
    // Four equal tiles in 80 cells by 20 (80 x 40 in layout space): squares there are 40
    // columns by 10 rows. In points they are square.
    for a in squarify(&[1, 1, 1, 1], CELLS.layout(area(0.0, 0.0, 80.0, 20.0))) {
        let r = CELLS.screen(a);
        assert_eq!((r.w, r.h), (40.0, 10.0), "{r:?}");
    }
    for a in squarify(&[1, 1, 1, 1], POINTS.layout(area(0.0, 0.0, 80.0, 80.0))) {
        let r = POINTS.screen(a);
        assert_eq!((r.w, r.h), (40.0, 40.0), "{r:?}");
    }
    let loose = Grid {
        snap: false,
        ..CELLS
    };
    let r = loose.screen(area(0.4, 1.0, 3.3, 5.0));
    assert_eq!(
        r,
        area(0.4, 0.5, 3.3, 2.5),
        "unsnapped: only the aspect is undone"
    );
}

#[test]
fn rows_become_tiles_inside_the_area_largest_first() {
    let s = map();
    let a = area(0.0, 2.0, 80.0, 20.0);
    let t = build(&s, a, &CELLS);
    let names: Vec<&str> = t.tops.iter().map(|&i| t.tiles[i].name.as_str()).collect();
    assert_eq!(names, ["big", "mid", "small"]);
    for tile in &t.tiles {
        assert!(a.contains(tile.rect.x, tile.rect.y), "{tile:?}");
        assert_eq!(tile.bound, a);
        assert_eq!(tile.level, 0, "depth 1 lays out the top level only");
        assert!(tile.room.label && tile.room.frame, "{tile:?}");
    }
    assert!(t.current(&s, a, &CELLS));
    assert!(!t.current(&s, area(0.0, 2.0, 81.0, 20.0), &CELLS));
    assert!(!t.current(&s, a, &POINTS));
}

#[test]
fn tiny_rows_merge_into_one_tile_and_the_largest_keeps_its_own() {
    let mut entries = vec![test_entry(0, "huge", 1_000_000)];
    entries.extend((1..200).map(|i| test_entry(i, &format!("t{i}"), 100)));
    let s = browser(entries);
    let t = build(&s, area(0.0, 0.0, 80.0, 20.0), &CELLS);
    let merged: Vec<&Tile> = t.tiles.iter().filter(|t| t.small > 0).collect();
    assert_eq!(merged.len(), 1);
    assert_eq!((merged[0].small, merged[0].size), (199, 19_900));
    assert_eq!(merged[0].node, None);
    assert_eq!(t.tiles[t.tops[0]].name, "huge");
    assert_eq!(t.tops.len(), 1, "the merged tile is not one of the rows");
}

#[test]
fn deeper_levels_nest_inside_the_inset_of_their_tile() {
    let mut s = map();
    s.map.depth = 2;
    for grid in [CELLS, POINTS] {
        let a = if grid == CELLS {
            area(0.0, 0.0, 80.0, 20.0)
        } else {
            area(10.0, 10.0, 800.0, 400.0)
        };
        let t = build(&s, a, &grid);
        let big = t.tops[0];
        assert!(t.tiles[big].nested, "big shows what is in it");
        let frame = t.tiles[big].rect;
        let inside: Vec<&Tile> = t.tiles.iter().filter(|t| t.up == Some(big)).collect();
        assert_eq!(inside.len(), 2);
        for tile in inside {
            assert_eq!((tile.level, tile.top), (1, 0));
            assert!(tile.rect.y >= frame.y + grid.inset.top, "{tile:?}");
            assert!(tile.rect.x >= frame.x + grid.inset.left, "{tile:?}");
            assert_eq!(tile.parent_size, t.tiles[big].size);
        }
    }
}

#[test]
fn peek_lays_out_the_selected_tile_alone() {
    let s = map();
    let t = build(&s, area(0.0, 0.0, 80.0, 20.0), &CELLS);
    let big = t.tops[0];
    let tiles = peek(&s, &t, big);
    assert!(tiles[0].nested && tiles[0].rect == t.tiles[big].rect);
    let names: Vec<&str> = tiles[1..].iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert!(tiles[1..].iter().all(|t| t.up == Some(0) && t.level == 1));
    // A file has nothing to show.
    assert!(peek(&s, &t, t.tops[2]).is_empty());
}

#[test]
fn arrows_move_to_the_nearest_tile_that_way() {
    let mut s = map();
    let t = build(&s, area(0.0, 0.0, 400.0, 200.0), &POINTS);
    assert_eq!(t.selected_top(&s), Some(0));
    // `big` is on the left, the others to its right.
    step(&mut s, &t, Toward::Left);
    assert_eq!(t.selected_top(&s), Some(0), "nothing further left");
    step(&mut s, &t, Toward::Right);
    let right = t.selected_top(&s).expect("a tile");
    assert_ne!(right, 0);
    step(&mut s, &t, Toward::Left);
    assert_eq!(t.selected_top(&s), Some(0));
    // A point finds the deepest tile under it.
    let mid = t.tiles[t.tops[1]].rect;
    let hit = t.at(mid.x + 1.0, mid.y + 1.0).expect("a tile");
    assert_eq!(t.tiles[hit].name, "mid");
    assert_eq!(t.at(-1.0, 0.0), None);
}

#[test]
fn labels_read_live_sizes_while_tiles_hold_still() {
    let mut s = map();
    let t = build(&s, area(0.0, 0.0, 80.0, 20.0), &CELLS);
    let big = &t.tiles[t.tops[0]];
    assert_eq!(big.live_size(&s), 6000);
    s.snap.entries[0].size = 9000;
    assert_eq!(
        big.live_size(&s),
        9000,
        "the entry as the snapshot has it now"
    );
    assert!(
        t.current(&s, t.area, &CELLS),
        "no new layout for a size alone"
    );
    let kid = &peek(&s, &t, t.tops[0])[1];
    assert_eq!((kid.live_size(&s), kid.whole(&s)), (4000, 6000));
}
