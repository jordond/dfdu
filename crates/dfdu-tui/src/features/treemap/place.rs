//! Turning rows and the levels read below them into tiles. [`build`] lays out the directory's
//! rows, and below each the levels the depth shows; at depth 1 only the selected tile shows
//! what is in it, laid out on its own by [`peek`] so that moving the selection does not lay
//! out the whole directory again.
//!
//! A tile that shows its children keeps a header row for its name, and in colour a margin of
//! one cell on its other sides, in which the selection's edge is drawn; without colour it keeps
//! the frame and a line under it (`Placer::inner`).

use std::collections::HashMap;

use dfdu_tree::NodeRef;
use ratatui::layout::Rect;

use super::layout::{Area, cells, squarify};
use super::{Key, Kids, MAX_TILES, MIN_CELLS, key_of};
use crate::app::App;
use crate::model::snapshot::Tag;
use crate::theme::ratio;

/// One tile of the layout.
#[derive(Clone, Debug)]
pub(super) struct Tile {
    pub rect: Rect,
    /// The area the tile was laid out in, with its siblings: a tile that does not reach its
    /// right edge leaves a gutter there.
    pub bound: Rect,
    /// 0 for the directory's own rows.
    pub level: u8,
    /// Index into `Layout::tops` of the top-level tile this one is in.
    pub top: usize,
    /// The tile this one is in, by index into the same list; `None` at the top level.
    pub up: Option<usize>,
    /// For top-level tiles: the row's entry, with its index into the snapshot when laid out.
    pub entry: Option<(NodeRef, usize)>,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Size of the directory the tile is in, for its share.
    pub parent: u64,
    pub mtime: i64,
    pub tag: Option<Tag>,
    /// The theme's category colour slot of the largest category among the children read, for
    /// a directory without a match of its own.
    pub dominant: Option<usize>,
    /// The tile of the entries merged for being too small: how many.
    pub small: u64,
    /// Drawn with its children inside.
    pub nested: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Layout {
    pub key: Key,
    /// What the levels below the top were read for when this was laid out.
    pub below: Option<Key>,
    pub area: Rect,
    /// Laid out for drawing without colour, with frames.
    pub framed: bool,
    pub tiles: Vec<Tile>,
    /// Tile index of each top-level tile, in layout order.
    pub tops: Vec<usize>,
}

/// What a tile is laid out from.
struct Item<'a> {
    entry: Option<(NodeRef, usize)>,
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
    framed: bool,
    rules: Option<&'r dfdu_rules::RuleSet>,
}

/// The smallest tile that shows its children.
const NEST_MIN: (u16, u16) = (8, 5);

impl Placer<'_> {
    /// Where the children of a tile at `r` go: below its header row with a cell of margin on
    /// the other sides, or inside its frame below the label line.
    fn inner(&self, r: Rect) -> Rect {
        if self.framed {
            Rect::new(r.x + 1, r.y + 2, r.width - 2, r.height - 3)
        } else {
            Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2)
        }
    }

    /// The colour slot of the category that holds the most of `kids` among those matched.
    fn dominant(&self, kids: Option<&Kids>) -> Option<usize> {
        let rules = self.rules?;
        let mut by: HashMap<usize, u64> = HashMap::new();
        for k in &kids?.items {
            if let Some(tag) = k.tag {
                *by.entry(tag.category(rules).1).or_default() += k.size;
            }
        }
        by.into_iter()
            .max_by_key(|&(slot, size)| (size, std::cmp::Reverse(slot)))
            .map(|(slot, _)| slot)
    }

    /// Turns the largest of `items` into tiles of `area`, merging those too small to label.
    /// `rest` is the merged count and size the caller already has; `up` is the tile they are
    /// in and `top` the top-level tile, `None` at the top level.
    fn place(
        &mut self,
        mut items: Vec<Item>,
        rest: (u64, u64),
        area: Rect,
        level: u8,
        up: Option<(usize, usize)>,
    ) {
        let cells_total = f64::from(area.width) * f64::from(area.height);
        let total: u64 = items.iter().map(|i| i.size).sum::<u64>() + rest.1;
        if total == 0 || area.is_empty() {
            return;
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.size));
        let fits = |s: u64| ratio(s, total) * cells_total >= MIN_CELLS;
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
        for (item, a) in items.into_iter().zip(squarify(&sizes, Area::of(area))) {
            let tile = cells(a);
            if tile.is_empty() {
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
                .filter(|_| tile.width >= NEST_MIN.0 && tile.height >= NEST_MIN.1);
            let dominant = if level == 0 && item.tag.is_none() {
                self.dominant(item.kids)
            } else {
                None
            };
            self.tiles.push(Tile {
                rect: tile,
                bound: area,
                level,
                top,
                up: up.map(|(i, _)| i),
                entry: item.entry,
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                parent: total,
                mtime: item.mtime,
                tag: item.tag,
                dominant,
                small: item.small,
                nested: nested.is_some(),
            });
            if let Some(kids) = nested {
                let inner = self.inner(tile);
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

/// Lays out the rows of the directory in `area`, and below them the levels the depth shows
/// (none at depth 1, where [`peek`] shows the selected tile's).
pub(super) fn build(app: &App, area: Rect, framed: bool) -> Layout {
    let key = key_of(app);
    let entries = &app.snap.entries;
    let below = &app.extras.treemap.below;
    let items = app
        .rows
        .iter()
        .map(|&i| {
            let e = &entries[i];
            Item {
                entry: Some((e.node, i)),
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
        depth: app.treemap.depth,
        framed,
        rules: app.snap.rules.as_deref(),
    };
    p.place(items, (0, 0), area, 0, None);
    Layout {
        key,
        below: app.extras.treemap.key,
        area,
        framed,
        tiles: p.tiles,
        tops: p.tops,
    }
}

/// At depth 1, the children of top-level tile `at` of `l` (the selected one), laid out inside
/// it, the tile itself marked as showing them; nothing when the tile is too small or nothing
/// was read below it.
pub(super) fn peek(app: &App, l: &Layout, at: usize) -> Vec<Tile> {
    let tile = &l.tiles[at];
    let Some((node, _)) = tile.entry else {
        return Vec::new();
    };
    let Some(kids) = app.extras.treemap.below.get(&node) else {
        return Vec::new();
    };
    if kids.items.is_empty() || tile.rect.width < NEST_MIN.0 || tile.rect.height < NEST_MIN.1 {
        return Vec::new();
    }
    let mut p = Placer {
        tiles: vec![Tile {
            nested: true,
            ..tile.clone()
        }],
        tops: Vec::new(),
        depth: 2,
        framed: l.framed,
        rules: app.snap.rules.as_deref(),
    };
    let inner = p.inner(tile.rect);
    p.place(
        Item::of_kids(kids),
        kids.rest,
        inner,
        1,
        Some((0, tile.top)),
    );
    p.tiles
}
