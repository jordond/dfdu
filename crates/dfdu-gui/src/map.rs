//! The Map view: the current directory as squarified tiles, from the session's tile tree
//! (`dfdu_session::treemap`) laid out in points ([`grid`]) and painted (`paint`).
//!
//! The tiles at the top level are the directory's rows, so the selection, marks, delete and
//! details are the list's. A click on a tile selects it, a double click opens it as `Enter`
//! does, and a right click opens its row's menu; the pointer over a tile names it with its size,
//! share and path. The arrows move between tiles (`keys`), Backspace goes up. At depth 1 only
//! the selected tile shows what is in it. Above the tiles, the colouring, the depth and what the
//! colours mean (`controls`).
//!
//! The tiles are laid out again only when the session's tree says they are stale or the area
//! changed, so they hold still while sizes stream in. Below [`MIN_SIZE`] the list is drawn
//! instead.

mod controls;
mod paint;

use dfdu_engine::tilde;
use dfdu_session::SessionState;
use dfdu_session::format::{count, human, percent};
use dfdu_session::palette::{Category, Token};
use dfdu_session::treemap::{self as map, AGES, Area, ColorBy, Grid, Inset, Tile};
use egui::{Align, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::browser::{Intent, ViewState};
use crate::kit;
use crate::list::category;
use crate::menu;
use crate::theme::Colors;
use paint::{HALF_GUTTER, HEADER, INSET};

/// Below this the list is drawn instead.
pub const MIN_SIZE: egui::Vec2 = vec2(240.0, 160.0);

/// The grid in points: square units on whole points, a tile of about 24 by 24 points or more
/// on its own, and its children below a header row for its name, inset on every side. Each
/// tile gives up [`HALF_GUTTER`] on every side for the gutters, which the thresholds count.
pub fn grid() -> Grid {
    let gutter = f64::from(2.0 * HALF_GUTTER);
    let inset = f64::from(INSET);
    Grid {
        aspect: 1.0,
        snap: true,
        min_area: 600.0,
        nest_min: (80.0, 64.0),
        label_min: (36.0 + gutter, 18.0 + gutter),
        frame_min: (6.0, 6.0),
        inset: Inset {
            left: inset,
            top: f64::from(HEADER) + inset,
            right: inset,
            bottom: inset,
        },
    }
}

/// The map has room in `ui`.
pub fn fits(ui: &Ui) -> bool {
    let room = ui.available_size();
    room.x >= MIN_SIZE.x && room.y >= MIN_SIZE.y
}

/// `r` as an area of the tile tree.
fn area_of(r: Rect) -> Area {
    Area {
        x: f64::from(r.min.x),
        y: f64::from(r.min.y),
        w: f64::from(r.width()),
        h: f64::from(r.height()),
    }
}

/// `a` on screen.
#[expect(clippy::cast_possible_truncation, reason = "a rectangle on screen")]
pub fn rect_of(a: Area) -> Rect {
    Rect::from_min_size(pos2(a.x as f32, a.y as f32), vec2(a.w as f32, a.h as f32))
}

/// Draws the map in all of `ui`'s room: the controls row, then the tiles.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let full = ui.available_rect_before_wrap();
    let controls_at = Rect::from_min_size(full.min, vec2(full.width(), controls::HEIGHT));
    // The outer gutters reach into the sheet's margin, so the tiles line up with the controls.
    let tiles_at = Rect::from_min_max(
        pos2(full.min.x - HALF_GUTTER, controls_at.max.y),
        full.max + vec2(HALF_GUTTER, HALF_GUTTER),
    );
    ui.allocate_rect(full, Sense::hover());
    let grid = grid();
    let area = area_of(tiles_at);
    if !(view.map.as_ref()).is_some_and(|t| t.current(s, area, &grid)) {
        view.map = Some(map::build(s, area, &grid));
    }
    let Some(tree) = view.map.as_ref() else {
        return;
    };
    let sel = tree.selected_top(s);
    // At depth 1 the selected tile shows what is in it, in place of its tile.
    let peeked = sel
        .filter(|_| s.map.depth == 1)
        .map(|top| (tree.tops[top], map::peek(s, tree, tree.tops[top])))
        .filter(|(_, tiles)| !tiles.is_empty());
    let mut layers = vec![(&tree.tiles[..], peeked.as_ref().map(|(at, _)| *at))];
    if let Some((_, tiles)) = &peeked {
        layers.push((&tiles[..], None));
    }
    let painter = paint::Painter::new(s, colors, sel);
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(controls_at)
            .layout(Layout::left_to_right(Align::Center)),
    );
    controls::show(&mut row, s, colors, &painter, &layers, out);
    if tree.tiles.is_empty() {
        let text = if s.filter.active() {
            "Nothing here matches the filter."
        } else {
            "This directory is empty."
        };
        let text = kit::sans(text, 13.0).color(colors.fg(Token::FgDim));
        ui.put(tiles_at, egui::Label::new(text));
        return;
    }
    for &(tiles, skip) in &layers {
        for i in (0..tiles.len()).filter(|&i| Some(i) != skip) {
            painter.tile(ui, tiles, i);
        }
    }
    painter.outline_selected(ui, tree);
    let hover = ui.ctx().pointer_hover_pos();
    for (top, &t) in tree.tops.iter().enumerate() {
        let tile = &tree.tiles[t];
        let id = ui.scope_id().with(("tile", top));
        let r = ui.interact(rect_of(tile.rect), id, Sense::click());
        let label = name_of(tile);
        r.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, sel == Some(top), &label)
        });
        if r.hovered() {
            painter.outline_hovered(ui, tile);
        }
        let r = r.on_hover_ui_at_pointer(|ui| {
            if let Some((tiles, i)) = hover.and_then(|p| under(&layers, p.x, p.y)) {
                tooltip(ui, s, colors, tiles, i);
            }
        });
        if r.double_clicked() {
            out.push(Intent::Tile { top, open: true });
        } else if r.clicked() || r.secondary_clicked() {
            out.push(Intent::Tile { top, open: false });
        }
        if let Some((_, at)) = tile.entry
            && let Some(row) = s.list.rows.iter().position(|&i| i == at)
        {
            let e = &s.snap.entries[at];
            r.context_menu(|ui| menu::row(ui, s, e, row, out));
        }
    }
    // The tiles merged for being small: named under the pointer, with nothing to select.
    for (i, tile) in tree.tiles.iter().enumerate() {
        if tile.level == 0 && tile.small > 0 {
            let id = ui.scope_id().with(("small", i));
            let r = ui.interact(rect_of(tile.rect), id, Sense::hover());
            r.on_hover_ui_at_pointer(|ui| tooltip(ui, s, colors, &tree.tiles, i));
        }
    }
}

/// The deepest tile at `(x, y)` among `layers`, the one on top first.
fn under<'t>(
    layers: &[(&'t [Tile], Option<usize>)],
    x: f32,
    y: f32,
) -> Option<(&'t [Tile], usize)> {
    let (x, y) = (f64::from(x), f64::from(y));
    layers.iter().rev().find_map(|&(tiles, skip)| {
        (0..tiles.len())
            .rev()
            .filter(|&i| Some(i) != skip)
            .find(|&i| tiles[i].rect.contains(x, y))
            .map(|i| (tiles, i))
    })
}

/// A tile's name as it is labelled: `/` after a directory's.
fn name_of(tile: &Tile) -> String {
    format!("{}{}", tile.name, if tile.is_dir { "/" } else { "" })
}

/// What the pointer over tile `i` of `tiles` shows: its name, its path, its size, share and
/// items, and its category with its safety; its age band when coloured by age.
fn tooltip(ui: &mut Ui, s: &SessionState, colors: &Colors, tiles: &[Tile], i: usize) {
    let tile = &tiles[i];
    let (fg, dim) = (colors.fg(Token::Fg), colors.fg(Token::FgDim));
    ui.spacing_mut().item_spacing.y = 4.0;
    if tile.small > 0 {
        ui.label(kit::mono_bold(format!("+{}", count(tile.small)), 15.0).color(fg));
        let text = format!("{} more entries, each too small for a tile", tile.small);
        ui.label(kit::sans(text, 12.0).color(dim));
        ui.label(kit::mono(human(tile.size), 12.0).color(fg));
        return;
    }
    ui.label(kit::mono_bold(name_of(tile), 15.0).color(fg));
    let mut names = Vec::new();
    let mut at = Some(i);
    while let Some(t) = at {
        names.push(tiles[t].name.as_bytes());
        at = tiles[t].up;
    }
    let mut path = s.snap.dir_path.clone();
    for name in names.iter().rev() {
        path.push(name);
    }
    ui.label(kit::mono(tilde(&path, s.home.as_deref()), 11.0).color(dim));
    let size = tile.live_size(s);
    let whole = tile.whole(s).max(1);
    let mut facts = format!("{} · {:.1}%", human(size), percent(size, whole));
    if let Some(n) = items(s, tile) {
        facts.push_str(&format!(
            " · {} item{}",
            count(n),
            if n == 1 { "" } else { "s" }
        ));
    }
    ui.label(kit::mono(facts, 12.0).color(fg));
    if let Some((name, slot, safety)) = category_of(s, tile) {
        ui.horizontal(|ui| {
            kit::chip(ui, name, colors.fg(category(slot)));
            if let Some(safety) = safety {
                ui.label(kit::sans(safety, 12.0).color(dim));
            }
        });
    }
    if map::color_shown(s) == ColorBy::Age {
        let band = map::age_band(tile.mtime, dfdu_tree::now_secs());
        ui.label(kit::sans(format!("age: {}", AGES[band].1), 12.0).color(dim));
    }
}

/// How many items a top-level directory's tile holds, as its row has it.
fn items(s: &SessionState, tile: &Tile) -> Option<u64> {
    let (node, at) = tile.entry?;
    let e = s.snap.entries.get(at).filter(|e| e.node == node)?;
    e.is_dir.then_some(e.items)
}

/// A tile's category, its colour slot, and its safety when it is its own match: the match's,
/// or for a directory without one the category most of what is matched inside it is in.
fn category_of(
    s: &SessionState,
    tile: &Tile,
) -> Option<(&'static str, usize, Option<&'static str>)> {
    if let (Some(rules), Some(tag)) = (&s.snap.rules, tile.tag) {
        let (name, slot) = tag.category(rules);
        let safety = (!tag.inherited).then(|| tag.class.safety.as_str());
        return Some((name, slot, safety));
    }
    let slot = tile.dominant?;
    let c = Category::ALL.get(slot).copied().unwrap_or(Category::Other);
    Some((paint::category_name(c), slot, None))
}
