//! The Map view (`2`): the current directory as squarified tiles.
//!
//! The tiles at the top level are the directory's rows, so the selection, marks, delete, details
//! and filter are the list's. At depth 1 (the default) only the selected tile shows what is in
//! it: what `Enter` opens. `+` shows up to [`MAX_DEPTH`] levels in every tile. The levels below
//! the top are read from the engine model ([`read`]) for the largest directories only, one
//! level even at depth 1, for the selected tile. A directory without a match of its own is
//! coloured by the category most of what is matched inside it is in, however deep
//! ([`dominant`]). The layout is recomputed when the list re-sorts (`App::sort_gen`), when the
//! area or the depth changes, when the tree's matches change (a scan classifies as it goes),
//! and otherwise kept, so tiles do not jump while sizes stream in; the labels of top-level
//! tiles show live sizes.
//!
//! Tiles too small to stay on their own are merged into one tile per parent. Below 60x15 cells
//! the list is drawn instead.

mod draw;
mod fill;
mod glyph;
pub mod layout;
mod paint;
mod place;
mod tile;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{Class, Ix, NodeRef};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Position, Rect};

use crate::app::keys::{self, Action};
use crate::app::views::{self, View};
use crate::app::{App, Effect};
use crate::model::snapshot::{Name, Tag, class_below, tag_of};
use place::Layout;

pub use draw::draw;

pub const MIN_DEPTH: u8 = 1;
pub const MAX_DEPTH: u8 = 3;
/// Below this the list is drawn instead.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 15;
/// At most this many tiles per parent before the rest is merged...
const MAX_TILES: usize = 48;
/// ...and a tile needs this many cells (6 by 3) to stay on its own.
const MIN_CELLS: f64 = 18.0;
/// Children the model read looks at in one read, over all directories. A directory that would
/// go over it is drawn without its contents.
const READ_BUDGET: usize = 250_000;
/// A second click on the same tile within this long zooms into it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What the tile colours show; `p` steps through them in this order.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ColorBy {
    /// The category of the rule each tile matched, or of the match it is in; a directory
    /// without one takes the category most of it is in.
    #[default]
    Category,
    /// How long ago each tile was modified.
    Age,
    /// Each top-level tile and everything inside it in its own colour.
    Folder,
}

impl ColorBy {
    pub fn label(self) -> &'static str {
        match self {
            ColorBy::Folder => "top folder",
            ColorBy::Age => "age",
            ColorBy::Category => "category",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Treemap {
    /// Levels shown, [`MIN_DEPTH`] to [`MAX_DEPTH`].
    pub depth: u8,
    pub color: ColorBy,
    /// The last frame drew the treemap (the view is on and the terminal is large enough).
    pub shown: bool,
    layout: Option<Layout>,
    last_click: Option<(Instant, NodeRef)>,
}

impl Default for Treemap {
    fn default() -> Self {
        Treemap {
            depth: 1,
            color: ColorBy::default(),
            shown: false,
            layout: None,
            last_click: None,
        }
    }
}

/// What a read was for: when it changes, the levels below the top are read again.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Key {
    dir: Option<NodeRef>,
    sort_gen: u64,
    depth: u8,
    mode: SizeMode,
    hide_hidden: bool,
    /// Hash of the type filter.
    types: u64,
}

fn key_of(app: &App) -> Key {
    Key {
        dir: app.snap.dir_node,
        sort_gen: app.sort_gen,
        depth: app.treemap.depth,
        mode: app.size_mode,
        hide_hidden: app.hide_hidden,
        types: {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            app.types.hash(&mut h);
            h.finish()
        },
    }
}

/// The levels below the top-level tiles, as the last read found them.
#[derive(Clone, Debug, Default)]
pub struct Info {
    key: Option<Key>,
    /// Shared, so that the copy each snapshot read makes is cheap.
    below: Arc<HashMap<NodeRef, Kids>>,
    /// The category colour slot of most of what is matched below each directory under the
    /// current one ([`dominant`]).
    dominant: Arc<HashMap<NodeRef, usize>>,
    /// How many nodes the tree had classified: matches found or decided after the read (a scan
    /// classifies as it goes) make it read again.
    classes: usize,
}

/// The largest children of a directory, and how many others there are and their total size.
#[derive(Clone, Debug, Default)]
pub struct Kids {
    items: Vec<Kid>,
    rest: (u64, u64),
}

#[derive(Clone, Debug)]
pub struct Kid {
    node: NodeRef,
    name: Name,
    is_dir: bool,
    size: u64,
    mtime: i64,
    tag: Option<Tag>,
    kids: Option<Kids>,
}

/// True when the treemap needs the levels below the top read again.
pub fn stale(app: &App) -> bool {
    on(app) && app.extras.treemap.key != Some(key_of(app))
}

/// Reads the levels below the largest top-level directories. Runs under the read lock and
/// visits at most [`READ_BUDGET`] children; nothing when the view is off or the last read is
/// still current.
pub fn read(m: &Model, app: &App) -> Info {
    if !on(app) {
        return Info::default();
    }
    let key = key_of(app);
    let classes = m.tree.classes().count();
    let last = &app.extras.treemap;
    if last.key == Some(key) && last.classes == classes {
        return last.clone();
    }
    let mut below = HashMap::new();
    // One level even at depth 1: what the selected tile shows.
    let levels = app.treemap.depth.max(2) - 1;
    let dir = app.snap.dir_node.and_then(|n| m.tree.resolve_visible(n));
    let info = |below| Info {
        key: Some(key),
        below: Arc::new(below),
        dominant: Arc::new(dir.map_or_else(HashMap::new, |d| dominant(m, d, key.mode))),
        classes,
    };
    let entries = &app.snap.entries;
    let mut dirs: Vec<(u64, usize)> = app
        .rows
        .iter()
        .filter(|&&i| entries[i].is_dir && !entries[i].pseudo)
        .map(|&i| (entries[i].size, i))
        .collect();
    largest_first(&mut dirs, MAX_TILES);
    let mut budget = READ_BUDGET;
    for (_, i) in dirs {
        let node = entries[i].node;
        if let Some(ix) = m.tree.resolve_visible(node)
            && let Some(kids) = read_kids(
                m,
                ix,
                m.rules
                    .as_ref()
                    .and_then(|r| r.class_in_effect(&m.tree, ix)),
                levels,
                &key,
                &app.types,
                &mut budget,
            )
        {
            below.insert(node, kids);
        }
    }
    info(below)
}

/// The category colour slot that holds the most matched bytes below each directory under
/// `dir`, from the tree's matches: a directory without a match of its own takes the category of
/// most of what is matched inside it, however deep (a project is its `node_modules/`). A match
/// inside another counts as part of the outer one. One walk up from each match.
fn dominant(m: &Model, dir: Ix, mode: SizeMode) -> HashMap<NodeRef, usize> {
    let Some(rules) = m.rules.as_deref() else {
        return HashMap::new();
    };
    let t = &m.tree;
    let mut by: HashMap<u32, HashMap<usize, u64>> = HashMap::new();
    'matches: for (c, class) in t.classes() {
        let mut ups = Vec::new();
        let mut at = t.parent(c);
        loop {
            match at {
                // Not below `dir`.
                None => continue 'matches,
                Some(a) if a == dir => break,
                // Inside another match, which holds it.
                Some(a) if t.class(a).is_some() => continue 'matches,
                Some(a) => {
                    ups.push(a);
                    at = t.parent(a);
                }
            }
        }
        let slot = rules.rule(class.rule).category().color as usize;
        let size = mode.of(&t.total(c));
        for a in ups {
            *by.entry(a.0).or_default().entry(slot).or_default() += size;
        }
    }
    by.into_iter()
        .filter_map(|(a, slots)| {
            let best = slots
                .into_iter()
                .max_by_key(|&(slot, size)| (size, std::cmp::Reverse(slot)))?;
            Some((t.node_ref(Ix(a)), best.0))
        })
        .collect()
}

/// Keeps the `n` largest of `v`, largest first.
fn largest_first<T: Ord>(v: &mut Vec<(u64, T)>, n: usize) {
    let by_size = |a: &(u64, T), b: &(u64, T)| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1));
    if v.len() > n {
        v.select_nth_unstable_by(n, by_size);
        v.truncate(n);
    }
    v.sort_unstable_by(by_size);
}

/// The children of `dir`, `levels` deep; `inside` is the class in effect for them. With a type
/// filter, sizes count the matching files only and children without any are left out.
fn read_kids(
    m: &Model,
    dir: Ix,
    inside: Option<(Ix, Class)>,
    levels: u8,
    key: &Key,
    types: &TypeFilter,
    budget: &mut usize,
) -> Option<Kids> {
    let tree = &m.tree;
    let children = tree.children(dir);
    if levels == 0 || children.len() > *budget {
        return None;
    }
    *budget -= children.len();
    let mut all: Vec<(u64, u32)> = children
        .filter(|&c| !(key.hide_hidden && tree.name(c).first() == Some(&b'.')))
        .filter_map(|c| {
            if types.is_empty() {
                return Some((key.mode.of(&tree.total(c)), c.0));
            }
            let t = filetype::sum(tree, c, types, None);
            (t.items > 0).then(|| (key.mode.of(&t), c.0))
        })
        .collect();
    let total: u64 = all.iter().map(|a| a.0).sum();
    let listed = all.len() as u64;
    largest_first(&mut all, MAX_TILES);
    let shown: u64 = all.iter().map(|a| a.0).sum();
    let rest_n = if types.is_empty() {
        tree.children(dir).len() as u64 - all.len() as u64
    } else {
        listed - all.len() as u64
    };
    let items = all
        .into_iter()
        .map(|(size, c)| {
            let c = Ix(c);
            let is_dir = tree.dir(c).is_some();
            Kid {
                node: tree.node_ref(c),
                name: tree.name(c).into(),
                is_dir,
                size,
                mtime: tree.mtime(c),
                tag: tag_of(m, inside, c),
                kids: if is_dir {
                    let below = class_below(m, inside, c);
                    read_kids(m, c, below, levels - 1, key, types, budget)
                } else {
                    None
                },
            }
        })
        .collect();
    Some(Kids {
        items,
        rest: (rest_n, total - shown),
    })
}

/// The view is the treemap (otherwise the list), also while the terminal is too small for it.
pub fn on(app: &App) -> bool {
    views::current(app) == View::Map
}

/// True when keys and the mouse act on the treemap: it is on and was drawn last frame.
pub fn active(app: &App) -> bool {
    on(app) && app.treemap.shown
}

/// The view was opened ([`views::switch`]): a click before it does not count towards a double
/// click.
pub fn on_open(app: &mut App) {
    app.treemap.last_click = None;
}

/// One level more or less, within [`MIN_DEPTH`] and [`MAX_DEPTH`].
pub fn deeper(app: &mut App, by: i8) {
    let t = &mut app.treemap;
    t.depth = t
        .depth
        .saturating_add_signed(by)
        .clamp(MIN_DEPTH, MAX_DEPTH);
    app.status = Some(format!("map depth {}", t.depth));
}

/// The colouring drawn: by category needs the rules, and without them is by top folder.
pub fn color_shown(app: &App) -> ColorBy {
    match app.treemap.color {
        ColorBy::Category if app.snap.rules.is_none() => ColorBy::Folder,
        c => c,
    }
}

/// `p`: the next colouring, category, age, then top folder (category only with rules).
pub fn cycle_color(app: &mut App) {
    let next = match color_shown(app) {
        ColorBy::Category => ColorBy::Age,
        ColorBy::Age => ColorBy::Folder,
        ColorBy::Folder if app.snap.rules.is_some() => ColorBy::Category,
        ColorBy::Folder => ColorBy::Age,
    };
    app.treemap.color = next;
    app.status = Some(format!("map colour: {}", next.label()));
}

/// The top-level tile of the selection, as the last frame laid it out.
fn selected_top(app: &App) -> Option<usize> {
    let l = app.treemap.layout.as_ref()?;
    let node = app.selected.as_ref()?.node?;
    l.tops
        .iter()
        .position(|&t| l.tiles[t].entry.is_some_and(|(n, _)| n == node))
}

/// Selects the row of top-level tile `top`.
fn select_top(app: &mut App, top: usize) {
    let Some(l) = &app.treemap.layout else {
        return;
    };
    let Some((node, _)) = l.tops.get(top).and_then(|&t| l.tiles[t].entry) else {
        return;
    };
    let entries = &app.snap.entries;
    if let Some(row) = app.rows.iter().position(|&i| entries[i].node == node) {
        app.move_to(row);
    }
}

/// Centre of `r` in layout space, where rows count [`layout::CELL_ASPECT`] times a column.
fn centre(r: Rect) -> (f64, f64) {
    (
        f64::from(r.x) + f64::from(r.width) / 2.0,
        (f64::from(r.y) + f64::from(r.height) / 2.0) * layout::CELL_ASPECT,
    )
}

/// The top-level tile nearest to `from` whose centre lies in direction `(dx, dy)`.
fn neighbour(l: &Layout, from: usize, (dx, dy): (f64, f64)) -> Option<usize> {
    let (cx, cy) = centre(l.tiles[l.tops[from]].rect);
    l.tops
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != from)
        .filter_map(|(i, &t)| {
            let (x, y) = centre(l.tiles[t].rect);
            let (ox, oy) = (x - cx, y - cy);
            (ox * dx + oy * dy > 0.0).then_some((i, ox * ox + oy * oy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Keys the treemap takes before the action registry: arrows and `hjkl` move between tiles.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    let dir = match k.code {
        KeyCode::Left | KeyCode::Char('h') => (-1.0, 0.0),
        KeyCode::Right | KeyCode::Char('l') => (1.0, 0.0),
        KeyCode::Up | KeyCode::Char('k') => (0.0, -1.0),
        KeyCode::Down | KeyCode::Char('j') => (0.0, 1.0),
        _ => return None,
    };
    app.status = None;
    let next = app
        .treemap
        .layout
        .as_ref()
        .and_then(|l| match selected_top(app) {
            Some(from) => neighbour(l, from, dir),
            None => (!l.tops.is_empty()).then_some(0),
        });
    if let Some(top) = next {
        select_top(app, top);
    }
    Some(Vec::new())
}

/// A click on the treemap: selects the top-level tile under it; a second click zooms in.
/// `None` when the click is outside the tiles.
pub fn click(app: &mut App, at: Position, now: Instant) -> Option<Vec<Effect>> {
    let l = app.treemap.layout.as_ref()?;
    if !l.area.contains(at) {
        return None;
    }
    let tile = l.tiles.iter().rev().find(|t| t.rect.contains(at))?;
    let (node, _) = l.tiles[*l.tops.get(tile.top)?].entry?;
    // A "+N small" tile at the top level selects nothing.
    if tile.level == 0 && tile.entry.is_none() {
        return Some(Vec::new());
    }
    let top = tile.top;
    let again = app
        .treemap
        .last_click
        .is_some_and(|(t, n)| n == node && now.duration_since(t) <= DOUBLE_CLICK);
    app.status = None;
    select_top(app, top);
    if again {
        app.treemap.last_click = None;
        return Some(keys::run(app, Action::Enter));
    }
    app.treemap.last_click = Some((now, node));
    Some(Vec::new())
}

/// Whether the treemap fits a terminal of size `area`; the list is drawn otherwise.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

#[cfg(test)]
pub(crate) mod tests;
