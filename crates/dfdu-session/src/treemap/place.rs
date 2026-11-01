//! The tile tree of the Map view, in the front end's units ([`Grid`]): [`build`] lays out the
//! directory's rows and below each the levels the depth shows; at depth 1 only the selected tile
//! shows what is in it, laid out on its own by [`peek`] so that moving the selection does not lay
//! out the whole directory again. The same tree serves terminal cells and window points: the
//! front end says how tall a unit is, whether edges snap to whole units, and how much room a
//! tile needs to stay on its own, to show its children, a label and a frame.
//!
//! A tile that shows its children keeps an inset for its name and edge ([`Inset`]); the
//! children are laid out in what is left.

use std::collections::HashMap;

use dfdu_tree::NodeRef;

use super::{Area, Key, Kids, MAX_TILES, key_of, squarify};
use crate::format::ratio;
use crate::snapshot::Tag;
use crate::state::SessionState;

/// What a front end's units need of a layout. Lengths are in its units: cells, or points.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Grid {
    /// Height of a unit in units of its width: about 2 for a terminal cell, 1 for a point.
    /// Tiles are square on screen, not in units.
    pub aspect: f64,
    /// Round each edge to the nearest whole unit, so neighbouring tiles stay flush with no gaps
    /// or overlaps.
    pub snap: bool,
    /// The area a tile needs to stay on its own; smaller ones are merged into one per parent.
    pub min_area: f64,
    /// The smallest tile, across and down, that shows its children.
    pub nest_min: (f64, f64),
    /// The smallest tile with room for a label ([`Room::label`]).
    pub label_min: (f64, f64),
    /// The smallest tile with room for a frame ([`Room::frame`]).
    pub frame_min: (f64, f64),
    /// Where the children of a tile go inside it.
    pub inset: Inset,
}

/// How far in from each edge of a tile its children are laid out: the top holds its name.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Inset {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Grid {
    /// `a` in layout space, where a unit down counts [`Grid::aspect`] times a unit across.
    #[must_use]
    pub fn layout(&self, a: Area) -> Area {
        Area {
            x: a.x,
            y: a.y * self.aspect,
            w: a.w,
            h: a.h * self.aspect,
        }
    }

    /// The rectangle layout-space area `a` covers in units, each edge rounded to the nearest
    /// whole unit when the grid snaps. Rounding edges rather than sizes keeps tiles flush.
    #[must_use]
    pub fn screen(&self, a: Area) -> Area {
        if !self.snap {
            return Area {
                x: a.x,
                y: a.y / self.aspect,
                w: a.w,
                h: a.h / self.aspect,
            };
        }
        let x0 = a.x.round();
        let x1 = (a.x + a.w).round();
        let y0 = (a.y / self.aspect).round();
        let y1 = ((a.y + a.h) / self.aspect).round();
        Area {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }

    /// Where the children of a tile at `r` go.
    fn inner(&self, r: Area) -> Area {
        let i = self.inset;
        Area {
            x: r.x + i.left,
            y: r.y + i.top,
            w: (r.w - i.left - i.right).max(0.0),
            h: (r.h - i.top - i.bottom).max(0.0),
        }
    }

    /// `r` is at least `min` across and down.
    fn fits(r: Area, min: (f64, f64)) -> bool {
        r.w >= min.0 && r.h >= min.1
    }
}

impl Area {
    /// It covers nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// The point `(x, y)` is inside it: on its left or top edge, or between its edges.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// What a tile has room for, by the thresholds of its [`Grid`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Room {
    /// A label: its name, its size below.
    pub label: bool,
    /// A frame round it.
    pub frame: bool,
}

/// One tile of the tree.
#[derive(Clone, Debug)]
pub struct Tile {
    /// Where it is, in units.
    pub rect: Area,
    /// The area it was laid out in, with its siblings: a tile that does not reach its right
    /// edge leaves a gutter there.
    pub bound: Area,
    /// 0 for the directory's own rows.
    pub level: u8,
    /// Index into [`TileTree::tops`] of the top-level tile this one is in.
    pub top: usize,
    /// The tile this one is in, by index into the same list; `None` at the top level.
    pub up: Option<usize>,
    /// For top-level tiles: the row's entry, with its index into the snapshot when laid out.
    pub entry: Option<(NodeRef, usize)>,
    /// Its node; `None` for the tile of the entries merged.
    pub node: Option<NodeRef>,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Size of the directory the tile is in, for its share.
    pub parent_size: u64,
    pub mtime: i64,
    pub tag: Option<Tag>,
    /// The category colour slot of most of what is matched inside, for a directory without a
    /// match of its own.
    pub dominant: Option<usize>,
    /// The tile of the entries merged for being too small: how many.
    pub small: u64,
    /// Drawn with its children inside.
    pub nested: bool,
    pub room: Room,
}

impl Tile {
    /// A cleanup candidate, marked as one where nothing is drawn over it: a match safe or
    /// regenerable that does not show its children.
    #[must_use]
    pub fn candidate(&self) -> bool {
        !self.nested && self.tag.is_some_and(Tag::candidate)
    }
}

/// The tiles of the current directory, as [`build`] laid them out, and what for.
#[derive(Clone, Debug)]
pub struct TileTree {
    pub tiles: Vec<Tile>,
    /// Tile index of each top-level tile, in layout order (largest first).
    pub tops: Vec<usize>,
    /// Laid out in this area with this grid.
    pub area: Area,
    pub grid: Grid,
    /// What the view showed when this was laid out.
    pub key: Key,
    /// What the levels below the top were read for, and how many matches the tree had then.
    pub below: Option<Key>,
    pub classes: usize,
}

/// A direction the arrow keys move the selection in, between top-level tiles.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Toward {
    Left,
    Right,
    Up,
    Down,
}

impl Toward {
    fn vector(self) -> (f64, f64) {
        match self {
            Toward::Left => (-1.0, 0.0),
            Toward::Right => (1.0, 0.0),
            Toward::Up => (0.0, -1.0),
            Toward::Down => (0.0, 1.0),
        }
    }
}

impl TileTree {
    /// It still shows the view as it stands, laid out in `area` with `grid`. Otherwise the
    /// rows re-sorted, the depth, the size mode, the levels read or the matches changed, and it
    /// is laid out again; until then tiles hold still while sizes stream in.
    #[must_use]
    pub fn current(&self, s: &SessionState, area: Area, grid: &Grid) -> bool {
        let below = &s.extras.treemap;
        self.key == key_of(s)
            && self.area == area
            && self.grid == *grid
            && self.below == below.key
            && self.classes == below.classes
    }

    /// The top-level tile of the selection.
    #[must_use]
    pub fn selected_top(&self, s: &SessionState) -> Option<usize> {
        let node = s.selected.as_ref()?.node?;
        (self.tops.iter()).position(|&t| self.tiles[t].entry.is_some_and(|(n, _)| n == node))
    }

    /// The deepest tile at `(x, y)`, by index into [`TileTree::tiles`].
    #[must_use]
    pub fn at(&self, x: f64, y: f64) -> Option<usize> {
        (0..self.tiles.len())
            .rev()
            .find(|&i| self.tiles[i].rect.contains(x, y))
    }

    /// Centre of top-level tile `top` in layout space, where tiles are square.
    fn centre(&self, top: usize) -> (f64, f64) {
        let r = self.tiles[self.tops[top]].rect;
        (r.x + r.w / 2.0, (r.y + r.h / 2.0) * self.grid.aspect)
    }

    /// The top-level tile nearest to top-level tile `from` whose centre lies `toward` it.
    #[must_use]
    pub fn neighbour(&self, from: usize, toward: Toward) -> Option<usize> {
        let (dx, dy) = toward.vector();
        let (cx, cy) = self.centre(from);
        (0..self.tops.len())
            .filter(|&i| i != from)
            .filter_map(|i| {
                let (x, y) = self.centre(i);
                let (ox, oy) = (x - cx, y - cy);
                (ox * dx + oy * dy > 0.0).then_some((i, ox * ox + oy * oy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

/// Selects the row of top-level tile `top` of `tree`.
pub fn select_top(s: &mut SessionState, tree: &TileTree, top: usize) {
    let Some((node, _)) = tree.tops.get(top).and_then(|&t| tree.tiles[t].entry) else {
        return;
    };
    let entries = &s.snap.entries;
    if let Some(row) = (s.list.rows.iter()).position(|&i| entries[i].node == node) {
        s.move_to(row);
    }
}

/// An arrow key: moves the selection to the nearest top-level tile `toward` the selected one,
/// or to the first tile when none is selected. The status line is cleared.
pub fn step(s: &mut SessionState, tree: &TileTree, toward: Toward) {
    s.status = None;
    let next = match tree.selected_top(s) {
        Some(from) => tree.neighbour(from, toward),
        None => (!tree.tops.is_empty()).then_some(0),
    };
    if let Some(top) = next {
        select_top(s, tree, top);
    }
}

/// What a tile is laid out from.
struct Item<'a> {
    entry: Option<(NodeRef, usize)>,
    node: Option<NodeRef>,
    name: String,
    is_dir: bool,
    size: u64,
    mtime: i64,
    tag: Option<Tag>,
    small: u64,
    kids: Option<&'a Kids>,
}

impl<'a> Item<'a> {
    fn of_kids(kids: &'a Kids) -> Vec<Item<'a>> {
        (kids.items.iter())
            .map(|k| Item {
                entry: None,
                node: Some(k.node),
                name: String::from_utf8_lossy(&k.name).into_owned(),
                is_dir: k.is_dir,
                size: k.size,
                mtime: k.mtime,
                tag: k.tag,
                small: 0,
                kids: k.kids.as_ref(),
            })
            .collect()
    }
}

/// Collects the tiles of a layout.
struct Placer<'r> {
    tiles: Vec<Tile>,
    tops: Vec<usize>,
    depth: u8,
    grid: &'r Grid,
    /// The main category below each directory.
    dominant: &'r HashMap<NodeRef, usize>,
}

impl Placer<'_> {
    /// Turns the largest of `items` into tiles of `area`, merging those too small to stay on
    /// their own. `rest` is the merged count and size the caller already has; `up` is the tile
    /// they are in and its top-level tile, `None` at the top level.
    fn place(
        &mut self,
        mut items: Vec<Item>,
        rest: (u64, u64),
        area: Area,
        level: u8,
        up: Option<(usize, usize)>,
    ) {
        let room = area.w * area.h;
        let total: u64 = items.iter().map(|i| i.size).sum::<u64>() + rest.1;
        if total == 0 || area.is_empty() {
            return;
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.size));
        let fits = |s: u64| ratio(s, total) * room >= self.grid.min_area;
        let keep = items
            .iter()
            .take(MAX_TILES)
            .take_while(|i| fits(i.size))
            .count()
            // Always one tile of its own, so the largest entry can be selected.
            .max(1.min(items.len()));
        let merged = items.split_off(keep);
        let (mut rest_n, mut rest_size) = rest;
        rest_n += merged.iter().map(|i| i.small.max(1)).sum::<u64>();
        rest_size += merged.iter().map(|i| i.size).sum::<u64>();
        if rest_size > 0 {
            items.push(Item {
                entry: None,
                node: None,
                name: String::new(),
                is_dir: false,
                size: rest_size,
                mtime: 0,
                tag: None,
                small: rest_n,
                kids: None,
            });
            items.sort_by_key(|i| std::cmp::Reverse(i.size));
        }
        let sizes: Vec<u64> = items.iter().map(|i| i.size).collect();
        let grid = *self.grid;
        for (item, a) in items.into_iter().zip(squarify(&sizes, grid.layout(area))) {
            let rect = grid.screen(a);
            if rect.is_empty() {
                continue;
            }
            let at = self.tiles.len();
            let top = up.map_or_else(
                || {
                    if item.entry.is_some() {
                        self.tops.push(at);
                    }
                    self.tops.len().saturating_sub(1)
                },
                |(_, top)| top,
            );
            let nested = item
                .kids
                .filter(|k| level + 1 < self.depth && !k.items.is_empty())
                .filter(|_| Grid::fits(rect, grid.nest_min));
            let dominant = (item.tag.is_none())
                .then(|| item.node.and_then(|n| self.dominant.get(&n).copied()))
                .flatten();
            self.tiles.push(Tile {
                rect,
                bound: area,
                level,
                top,
                up: up.map(|(i, _)| i),
                entry: item.entry,
                node: item.node,
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                parent_size: total,
                mtime: item.mtime,
                tag: item.tag,
                dominant,
                small: item.small,
                nested: nested.is_some(),
                room: room_of(rect, &grid),
            });
            if let Some(kids) = nested {
                let inner = grid.inner(rect);
                self.place(
                    Item::of_kids(kids),
                    kids.rest,
                    inner,
                    level + 1,
                    Some((at, top)),
                );
            }
        }
    }
}

/// What a tile at `r` has room for.
fn room_of(r: Area, grid: &Grid) -> Room {
    Room {
        label: Grid::fits(r, grid.label_min),
        frame: Grid::fits(r, grid.frame_min),
    }
}

/// Lays out the rows of the directory in `area`, and below them the levels the depth shows
/// (none at depth 1, where [`peek`] shows the selected tile's).
#[must_use]
pub fn build(s: &SessionState, area: Area, grid: &Grid) -> TileTree {
    let entries = &s.snap.entries;
    let below = &s.extras.treemap.below;
    let items = (s.list.rows.iter())
        .map(|&i| {
            let e = &entries[i];
            Item {
                entry: Some((e.node, i)),
                node: Some(e.node),
                name: String::from_utf8_lossy(&e.name).into_owned(),
                is_dir: e.is_dir,
                size: e.size,
                mtime: e.mtime,
                tag: e.tag,
                small: 0,
                kids: below.get(&e.node),
            }
        })
        .collect();
    let mut p = Placer {
        tiles: Vec::new(),
        tops: Vec::new(),
        depth: s.map.depth,
        grid,
        dominant: &s.extras.treemap.dominant,
    };
    p.place(items, (0, 0), area, 0, None);
    TileTree {
        tiles: p.tiles,
        tops: p.tops,
        area,
        grid: *grid,
        key: key_of(s),
        below: s.extras.treemap.key,
        classes: s.extras.treemap.classes,
    }
}

/// At depth 1, the children of tile `at` of `tree` (the selected one), laid out inside it, the
/// tile itself first and marked as showing them; nothing when the tile is too small or nothing
/// was read below it.
#[must_use]
pub fn peek(s: &SessionState, tree: &TileTree, at: usize) -> Vec<Tile> {
    let tile = &tree.tiles[at];
    let Some((node, _)) = tile.entry else {
        return Vec::new();
    };
    let Some(kids) = s.extras.treemap.below.get(&node) else {
        return Vec::new();
    };
    if kids.items.is_empty() || !Grid::fits(tile.rect, tree.grid.nest_min) {
        return Vec::new();
    }
    let mut p = Placer {
        tiles: vec![Tile {
            nested: true,
            ..tile.clone()
        }],
        tops: Vec::new(),
        depth: 2,
        grid: &tree.grid,
        dominant: &s.extras.treemap.dominant,
    };
    let inner = tree.grid.inner(tile.rect);
    p.place(
        Item::of_kids(kids),
        kids.rest,
        inner,
        1,
        Some((0, tile.top)),
    );
    p.tiles
}

#[cfg(test)]
mod tests;
