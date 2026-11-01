//! The Map view: the current directory as squarified tiles, from the session's tile tree
//! (`dfdu_session::treemap`) laid out in points ([`grid`]) and painted (`paint`).
//!
//! The tiles at the top level are the directory's rows, so the selection, marks, delete and
//! details are the list's. A click on a tile selects it, a double click opens it as `Enter`
//! does, and a right click opens its row's menu; the pointer over a tile names it with its size
//! and path. The arrows move between tiles (`keys`), Backspace goes up. At depth 1 only the
//! selected tile shows what is in it. Above the tiles, what the colours mean and the depth;
//! below, the selected tile's place among them.
//!
//! The tiles are laid out again only when the session's tree says they are stale or the area
//! changed, so they hold still while sizes stream in. Below [`MIN_SIZE`] the list is drawn
//! instead.

mod paint;

use dfdu_engine::tilde;
use dfdu_session::SessionState;
use dfdu_session::format::{human, percent};
use dfdu_session::palette::Token;
use dfdu_session::treemap::{self as map, AGES, Area, ColorBy, Grid, Inset, Tile, TileTree};
use egui::{Align, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::browser::{Intent, ViewState};
use crate::menu;
use crate::theme::Colors;

/// Below this the list is drawn instead.
pub const MIN_SIZE: egui::Vec2 = vec2(240.0, 160.0);
/// Room around the text in a tile's header.
const PAD: f32 = 3.0;

/// The grid in points for text rows `line` points high: square units on whole points, a tile
/// of about 30 by 20 points or more on its own, and its children below a header row for its
/// name with a margin on the other sides.
pub fn grid(line: f32) -> Grid {
    let head = f64::from(line + 2.0 * PAD);
    Grid {
        aspect: 1.0,
        snap: true,
        min_area: 600.0,
        nest_min: (80.0, head * 2.0 + 16.0),
        label_min: (40.0, head),
        frame_min: (6.0, 6.0),
        inset: Inset {
            left: 3.0,
            top: head,
            right: 3.0,
            bottom: 3.0,
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

/// Draws the map in all of `ui`'s room: the legend, the tiles, and the selected tile's place.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    view: &mut ViewState,
    out: &mut Vec<Intent>,
) {
    let full = ui.available_rect_before_wrap();
    let row = ui.spacing().interact_size.y;
    let gap = ui.spacing().item_spacing.y;
    let legend = Rect::from_min_size(full.min, vec2(full.width(), row));
    let info = Rect::from_min_max(pos2(full.min.x, full.max.y - row), full.max);
    let tiles_at = Rect::from_min_max(
        pos2(full.min.x, legend.max.y + gap),
        pos2(full.max.x, info.min.y - gap),
    );
    ui.allocate_rect(full, Sense::hover());
    let grid = grid(ui.text_style_height(&egui::TextStyle::Body));
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
    let painter = paint::Painter::new(ui, s, colors, sel);
    let mut child = |r: Rect| {
        ui.new_child(
            UiBuilder::new()
                .max_rect(r)
                .layout(Layout::left_to_right(Align::Center)),
        )
    };
    legend_line(&mut child(legend), s, colors, &painter, &layers, out);
    position_line(&mut child(info), s, colors, tree, sel);
    if tree.tiles.is_empty() {
        let text = if s.filter.active() {
            "Nothing here matches the filter."
        } else {
            "This directory is empty."
        };
        ui.put(tiles_at, egui::Label::new(colors.text(text, Token::FgDim)));
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

/// What the pointer over tile `i` of `tiles` shows: its name, size and share, and its path.
fn tooltip(ui: &mut Ui, s: &SessionState, colors: &Colors, tiles: &[Tile], i: usize) {
    let tile = &tiles[i];
    if tile.small > 0 {
        let text = format!("{} more entries, each too small for a tile", tile.small);
        ui.label(colors.text(text, Token::Fg));
        ui.label(colors.text(human(tile.size), Token::SizeBig));
        return;
    }
    ui.label(colors.text(name_of(tile), Token::Title));
    let size = tile.live_size(s);
    let whole = tile.whole(s).max(1);
    ui.label(colors.text(
        format!("{}  {:.1}%", human(size), percent(size, whole)),
        Token::SizeBig,
    ));
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
    ui.label(colors.text(tilde(&path, s.home.as_deref()), Token::FgDim));
    if let Some(text) = paint::third_line(s, tile, dfdu_tree::now_secs()) {
        ui.label(colors.text(text, Token::FgDim));
    }
}

/// The legend: the colouring, chosen here, and what its colours mean (the categories on
/// screen, or the age bands), the mark of cleanup candidates, and the depth on the right.
fn legend_line(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    painter: &paint::Painter,
    layers: &[(&[Tile], Option<usize>)],
    out: &mut Vec<Intent>,
) {
    let shown = map::color_shown(s);
    ui.label(colors.text("Colour:", Token::FgDim));
    for c in [ColorBy::Category, ColorBy::Age, ColorBy::Folder] {
        let enabled = c != ColorBy::Category || s.snap.rules.is_some();
        let r = ui.add_enabled(enabled, egui::Button::selectable(shown == c, c.label()));
        if r.clicked() && shown != c {
            out.push(Intent::MapColor(c));
        }
    }
    ui.separator();
    match shown {
        ColorBy::Category => {
            let mut slots = Vec::new();
            for &(tiles, _) in layers {
                for i in 0..tiles.len() {
                    if let Some(slot) = painter.base(tiles, i).1
                        && !slots.contains(&slot)
                    {
                        slots.push(slot);
                    }
                }
            }
            for slot in slots {
                let category = dfdu_session::palette::Category::ALL
                    .get(slot)
                    .copied()
                    .unwrap_or(dfdu_session::palette::Category::Other);
                painter.swatch(ui, colors.fg(category.token()), false);
                ui.label(colors.text(paint::category_name(category), Token::FgDim));
            }
        }
        ColorBy::Age => {
            for (band, (_, name)) in AGES.iter().enumerate() {
                painter.swatch(ui, colors.fg(map::age_token(band)), false);
                ui.label(colors.text(*name, Token::FgDim));
            }
        }
        ColorBy::Folder => {}
    }
    painter.swatch(ui, colors.fg(Token::FgDim), true);
    ui.label(colors.text("cleanup candidate", Token::FgDim));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let deeper = ui.add_enabled(s.map.depth < map::MAX_DEPTH, egui::Button::new("+"));
        if deeper.on_hover_text("One level deeper").clicked() {
            out.push(Intent::Act(dfdu_session::actions::Action::TreemapDeeper));
        }
        let less = ui.add_enabled(s.map.depth > map::MIN_DEPTH, egui::Button::new("-"));
        if less.on_hover_text("One level less").clicked() {
            out.push(Intent::Act(dfdu_session::actions::Action::TreemapShallower));
        }
        ui.label(colors.text(format!("Depth {}", s.map.depth), Token::FgDim));
    });
}

/// Below the tiles: how to use them, and the selected tile's place among the top-level tiles
/// in the order they were laid out (largest first).
fn position_line(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    tree: &TileTree,
    sel: Option<usize>,
) {
    let hint = "Click selects, double-click opens, Backspace goes up";
    ui.label(colors.text(hint, Token::FgDim));
    if let Some(at) = sel {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let place = format!("{}/{}", at + 1, tree.tops.len());
            ui.label(colors.text(place, Token::FgDim));
            if let Some(e) = s.selected_entry() {
                ui.label(colors.text(String::from_utf8_lossy(&e.name), Token::Fg));
            }
        });
    }
}
