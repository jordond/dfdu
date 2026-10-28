//! The treemap view (`2`): the current directory as nested squarified tiles.
//!
//! The tiles at the top level are the directory's rows, so the selection, marks, delete, details
//! and filter are the list's. Below them the view shows up to [`MAX_DEPTH`] levels, read from the
//! engine model ([`read`]) for the largest directories only. The layout is recomputed when the
//! list re-sorts (`App::sort_gen`), when the area or the depth changes, and otherwise kept, so
//! tiles do not jump while sizes stream in; the labels of top-level tiles show live sizes.
//!
//! Tiles too small to label are merged into one "+N small" tile per parent. Below 60x15 cells
//! the list is drawn instead.

pub mod layout;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dfdu_engine::Model;
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{Class, Ix, NodeRef, Tree};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
use crate::snapshot::{Name, SizeMode, Tag, tag_of};
use crate::theme::{count, human};
use layout::{Area, cells, squarify};

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

/// What the tile colours show.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Colour {
    /// Each top-level tile and everything inside it in its own colour.
    #[default]
    Folder,
    /// How long ago each tile was modified.
    Age,
    /// The category of the rule each tile matched, or of the match it is in; cleanup
    /// candidates hatched.
    Category,
}

impl Colour {
    fn label(self) -> &'static str {
        match self {
            Colour::Folder => "top folder",
            Colour::Age => "age",
            Colour::Category => "category",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Treemap {
    /// The view is the treemap (otherwise the list).
    pub on: bool,
    /// Levels shown, [`MIN_DEPTH`] to [`MAX_DEPTH`].
    pub depth: u8,
    pub colour: Colour,
    /// The last frame drew the treemap (the view is on and the terminal is large enough).
    pub shown: bool,
    layout: Option<Layout>,
    last_click: Option<(Instant, NodeRef)>,
}

impl Default for Treemap {
    fn default() -> Self {
        Treemap {
            on: false,
            depth: 2,
            colour: Colour::default(),
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
}

/// The largest children of a directory, and how many others there are and their total size.
#[derive(Clone, Debug, Default)]
pub struct Kids {
    items: Vec<Kid>,
    rest: (u64, u64),
}

#[derive(Clone, Debug)]
pub struct Kid {
    name: Name,
    is_dir: bool,
    size: u64,
    mtime: i64,
    tag: Option<Tag>,
    kids: Option<Kids>,
}

/// True when the treemap needs the levels below the top read again.
pub fn stale(app: &App) -> bool {
    app.treemap.on && app.extras.treemap.key != Some(key_of(app))
}

/// Reads the levels below the largest top-level directories. Runs under the read lock and
/// visits at most [`READ_BUDGET`] children; nothing when the view is off or the last read is
/// still current.
pub fn read(m: &Model, app: &App) -> Info {
    if !app.treemap.on {
        return Info::default();
    }
    let key = key_of(app);
    if app.extras.treemap.key == Some(key) {
        return app.extras.treemap.clone();
    }
    let mut below = HashMap::new();
    let levels = app.treemap.depth.saturating_sub(1);
    let info = |below| Info {
        key: Some(key),
        below: Arc::new(below),
    };
    if levels == 0 {
        return info(below);
    }
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
                &m.tree,
                ix,
                m.tree.class_in_effect(ix).map(|(_, c)| c),
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
    tree: &Tree,
    dir: Ix,
    inside: Option<Class>,
    levels: u8,
    key: &Key,
    types: &TypeFilter,
    budget: &mut usize,
) -> Option<Kids> {
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
                name: tree.name(c).into(),
                is_dir,
                size,
                mtime: tree.mtime(c),
                tag: tag_of(tree, inside, c),
                kids: if is_dir {
                    let below = inside.or_else(|| tree.class(c));
                    read_kids(tree, c, below, levels - 1, key, types, budget)
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

/// One tile of the layout.
#[derive(Clone, Debug)]
struct Tile {
    rect: Rect,
    /// 0 for the directory's own rows.
    level: u8,
    /// Index into `Layout::tops` of the top-level tile this one is in.
    top: usize,
    /// For top-level tiles: the row's entry, with its index into the snapshot when laid out.
    entry: Option<(NodeRef, usize)>,
    name: String,
    is_dir: bool,
    size: u64,
    /// Size of the directory the tile is in, for its share.
    parent: u64,
    mtime: i64,
    tag: Option<Tag>,
    /// "+N small": the number of entries merged into it.
    small: u64,
    /// Drawn with its children inside: a frame and a label line.
    framed: bool,
}

#[derive(Clone, Debug)]
struct Layout {
    key: Key,
    /// What the levels below the top were read for when this was laid out.
    below: Option<Key>,
    area: Rect,
    tiles: Vec<Tile>,
    /// Tile index of each top-level tile, in layout order.
    tops: Vec<usize>,
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

/// Collects the tiles of a layout.
struct Placer {
    tiles: Vec<Tile>,
    tops: Vec<usize>,
    depth: u8,
}

impl Placer {
    /// Turns the largest of `items` into tiles of `area`, merging those too small to label.
    /// `rest` is the merged count and size the caller already has; `top` is the top-level tile
    /// they are in, `None` at the top level.
    fn place(
        &mut self,
        mut items: Vec<Item>,
        rest: (u64, u64),
        area: Rect,
        level: u8,
        top: Option<usize>,
    ) {
        let cells_total = area.width as f64 * area.height as f64;
        let total: u64 = items.iter().map(|i| i.size).sum::<u64>() + rest.1;
        if total == 0 || area.is_empty() {
            return;
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.size));
        let fits = |s: u64| s as f64 / total as f64 * cells_total >= MIN_CELLS;
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
                name: format!("+{} small", count(rest_n)),
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
            let rect = cells(a);
            if rect.is_empty() {
                continue;
            }
            let top = top.unwrap_or_else(|| {
                if item.entry.is_some() {
                    self.tops.push(self.tiles.len());
                }
                self.tops.len().saturating_sub(1)
            });
            let nested = item
                .kids
                .filter(|k| level + 1 < self.depth && !k.items.is_empty())
                .filter(|_| rect.width >= 8 && rect.height >= 5);
            self.tiles.push(Tile {
                rect,
                level,
                top,
                entry: item.entry,
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                parent: total,
                mtime: item.mtime,
                tag: item.tag,
                small: item.small,
                framed: nested.is_some(),
            });
            if let Some(kids) = nested {
                // Inside the frame, below the label line.
                let inner = Rect::new(rect.x + 1, rect.y + 2, rect.width - 2, rect.height - 3);
                let items = kids.items.iter().map(|k| Item {
                    entry: None,
                    name: String::from_utf8_lossy(&k.name).into_owned(),
                    is_dir: k.is_dir,
                    size: k.size,
                    mtime: k.mtime,
                    tag: k.tag,
                    small: 0,
                    kids: k.kids.as_ref(),
                });
                self.place(items.collect(), kids.rest, inner, level + 1, Some(top));
            }
        }
    }
}

fn build(app: &App, area: Rect) -> Layout {
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
    };
    p.place(items, (0, 0), area, 0, None);
    Layout {
        key,
        below: app.extras.treemap.key,
        area,
        tiles: p.tiles,
        tops: p.tops,
    }
}

/// True when keys and the mouse act on the treemap: it is on and was drawn last frame.
pub fn active(app: &App) -> bool {
    app.treemap.on && app.treemap.shown
}

/// Switches between the list and the treemap.
pub fn show(app: &mut App, on: bool) {
    app.treemap.on = on;
    app.treemap.last_click = None;
}

/// One level more or less, within [`MIN_DEPTH`] and [`MAX_DEPTH`].
pub fn deeper(app: &mut App, by: i8) {
    let t = &mut app.treemap;
    t.depth = (t.depth as i8 + by).clamp(MIN_DEPTH as i8, MAX_DEPTH as i8) as u8;
    app.status = Some(format!("treemap depth {}", t.depth));
}

pub fn cycle_colour(app: &mut App) {
    let t = &mut app.treemap;
    t.colour = match t.colour {
        Colour::Folder => Colour::Age,
        Colour::Age if app.snap.rules.is_some() => Colour::Category,
        Colour::Age | Colour::Category => Colour::Folder,
    };
    app.status = Some(format!("treemap colour: {}", t.colour.label()));
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
        r.x as f64 + r.width as f64 / 2.0,
        (r.y as f64 + r.height as f64 / 2.0) * layout::CELL_ASPECT,
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
        return Some(keys::run(app, Action::Open, now));
    }
    app.treemap.last_click = Some((now, node));
    Some(Vec::new())
}

/// Whether the treemap fits a terminal of size `area`; the list is drawn otherwise.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

/// Age bands for [`Colour::Age`]: their upper bound in days and name.
const AGES: [(i64, &str); 4] = [
    (7, "week"),
    (90, "3 months"),
    (365, "year"),
    (i64::MAX, "older"),
];

fn age_band(mtime: i64, now: i64) -> usize {
    let days = now.saturating_sub(mtime) / 86_400;
    AGES.iter()
        .position(|&(d, _)| days < d)
        .unwrap_or(AGES.len() - 1)
}

fn age_style(app: &App, band: usize) -> Style {
    let t = &app.theme;
    [t.ok, t.fg, t.warn, t.dim][band]
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Draws the treemap in `area`: a legend line, the tiles, and a line on the selected entry.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant) {
    crate::motion::frame(app, now, &[]);
    app.list_height = area.height as usize;
    let [legend, map, info] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .areas(area);
    let key = key_of(app);
    let current = app
        .treemap
        .layout
        .as_ref()
        .is_some_and(|l| l.key == key && l.area == map && l.below == app.extras.treemap.key);
    if !current {
        app.treemap.layout = Some(build(app, map));
    }
    let today = now_secs();
    draw_legend(f, app, legend);
    let app = &*app;
    if let Some(line) = selected_line(app) {
        f.render_widget(Paragraph::new(line), info);
    }
    let Some(l) = &app.treemap.layout else {
        return;
    };
    if l.tiles.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("  (empty)", app.theme.dim)),
            map,
        );
        return;
    }
    let sel = selected_top(app);
    let buf = f.buffer_mut();
    for tile in &l.tiles {
        draw_tile(buf, app, tile, sel, today);
    }
}

fn draw_legend(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut spans = vec![Span::styled(
        format!(" colour: {}", app.treemap.colour.label()),
        t.dim,
    )];
    if app.treemap.colour == Colour::Category {
        spans.push(Span::styled("  ╱ cleanup candidate", t.dim));
    }
    if app.treemap.colour == Colour::Age {
        for (band, (_, name)) in AGES.iter().enumerate() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("■", age_style(app, band)));
            spans.push(Span::styled(format!(" {name}"), t.dim));
        }
    }
    spans.push(Span::styled(
        format!(
            "   {} colour  depth {} ({} {})",
            keys::first_key(Action::TreemapColour),
            app.treemap.depth,
            keys::first_key(Action::TreemapDeeper),
            keys::first_key(Action::TreemapShallower),
        ),
        t.dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn tile_style(app: &App, tile: &Tile, today: i64) -> Style {
    let t = &app.theme;
    if tile.small > 0 {
        return t.dim;
    }
    match app.treemap.colour {
        Colour::Folder => t.cat[tile.top % t.cat.len()],
        Colour::Age => age_style(app, age_band(tile.mtime, today)),
        Colour::Category => match (&app.snap.rules, tile.tag) {
            (Some(rules), Some(tag)) => t.cat[tag.category(rules).1],
            _ => t.dim,
        },
    }
}

/// The live size of a top-level tile's entry, or the size it was laid out with.
fn live_size(app: &App, tile: &Tile) -> u64 {
    match tile.entry {
        Some((node, i)) => app
            .snap
            .entries
            .get(i)
            .filter(|e| e.node == node)
            .map_or(tile.size, |e| e.size),
        None => tile.size,
    }
}

fn draw_tile(buf: &mut Buffer, app: &App, tile: &Tile, sel: Option<usize>, today: i64) {
    let r = tile.rect.intersection(buf.area);
    if r.is_empty() {
        return;
    }
    let style = tile_style(app, tile, today);
    let selected = tile.level == 0 && sel == Some(tile.top) && tile.entry.is_some();
    let marked = tile.entry.is_some_and(|(node, _)| app.marks.contains(node));
    let size = live_size(app, tile);
    let name = if tile.is_dir {
        format!("{}/", tile.name)
    } else {
        tile.name.clone()
    };
    let name = if marked { format!("✓ {name}") } else { name };
    // Top-level tiles show live sizes, so their share is of the directory's live total.
    let parent = match tile.level {
        0 => app.size_mode.of(&app.snap.dir_total),
        _ => tile.parent,
    };
    let share = if tile.small > 0 || parent == 0 {
        String::new()
    } else {
        format!("  {}%", size.saturating_mul(100) / parent)
    };
    let size_text = format!("{}{share}", human(size));
    let name_style = if tile.is_dir {
        app.theme.dir.patch(style)
    } else {
        style
    };
    if r.width >= 4 && r.height >= 3 {
        let border = if selected {
            app.theme.border_focus.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        Block::bordered()
            .border_type(if selected {
                BorderType::Thick
            } else {
                BorderType::Plain
            })
            .border_style(border)
            .render(r, buf);
        let mut inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
        // Cleanup candidates and what is in them are hatched, under the labels; a framed
        // tile's own tiles are.
        let candidate = tile.tag.is_some_and(|g| g.candidate());
        if app.treemap.colour == Colour::Category && candidate && !tile.framed {
            // Below the label lines: name, size, category.
            for y in (inner.top() + 3).min(inner.bottom())..inner.bottom() {
                for x in inner.left()..inner.right() {
                    if (x + y) % 2 == 0 {
                        buf[(x, y)].set_symbol("╱").set_style(style);
                    }
                }
            }
        }
        // One cell of padding when the name has room for it.
        if inner.width >= 6 {
            inner.x += 1;
            inner.width -= 1;
        }
        let w = inner.width as usize;
        // The share goes first when the size does not fit with it, then the size: a cut size
        // would read as another number.
        let size_text = [size_text, human(size)]
            .into_iter()
            .find(|s| Span::raw(s).width() <= w)
            .unwrap_or_default();
        let one_line = format!("{name}  {size_text}");
        if tile.framed || inner.height == 1 {
            let text = if Span::raw(&one_line).width() <= w {
                one_line
            } else {
                clip(&name, w)
            };
            buf.set_stringn(inner.x, inner.y, text, w, name_style);
        } else {
            buf.set_stringn(inner.x, inner.y, clip(&name, w), w, name_style);
            buf.set_stringn(inner.x, inner.y + 1, &size_text, w, app.theme.fg);
            if app.treemap.colour == Colour::Age && tile.small == 0 && inner.height >= 3 {
                let band = AGES[age_band(tile.mtime, today)].1;
                buf.set_stringn(inner.x, inner.y + 2, format!("≤ {band}"), w, app.theme.dim);
            }
            if app.treemap.colour == Colour::Category
                && inner.height >= 3
                && let (Some(rules), Some(tag)) = (&app.snap.rules, tile.tag)
                && !tag.inherited
            {
                let safety = dfdu_rules::Safety::ALL[tag.class.safety as usize].as_str();
                let text = format!("{} · {safety}", tag.category(rules).0);
                buf.set_stringn(inner.x, inner.y + 2, text, w, app.theme.dim);
            }
        }
    } else {
        // Too small for a frame: a shaded block with what fits of the name.
        let fill = if selected { app.theme.selected } else { style };
        for y in r.top()..r.bottom() {
            buf.set_stringn(r.x, y, "░".repeat(r.width as usize), r.width as usize, fill);
        }
        buf.set_stringn(
            r.x,
            r.y,
            clip(&name, r.width as usize),
            r.width as usize,
            fill,
        );
    }
}

/// `s` cut to `w` cells, ending in `…` when it was cut.
fn clip(s: &str, w: usize) -> String {
    if Span::raw(s).width() <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = Span::raw(c.to_string()).width();
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

/// The line under the treemap: the selected tile's name, size, share and items.
fn selected_line(app: &App) -> Option<Line<'static>> {
    let e = app.selected_entry()?;
    let total = app.size_mode.of(&app.snap.dir_total).max(1);
    let mut text = format!(
        " ▶ {}{}  {}  {:.1}%",
        String::from_utf8_lossy(&e.name),
        if e.is_dir { "/" } else { "" },
        human(e.size),
        e.size as f64 * 100.0 / total as f64
    );
    if e.is_dir {
        text.push_str(&format!("  ·  {} items", count(e.items)));
    }
    Some(Line::styled(text, app.theme.fg))
}

#[cfg(test)]
pub(crate) mod tests;
