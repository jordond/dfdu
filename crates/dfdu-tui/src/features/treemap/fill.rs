//! A tile in colour (`Paint::Fill`): filled, with a header row and the selection's edge.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::Span;

use super::draw::{
    LABEL_MIN, base_style, candidate, cells_of, fit_name, hatch, put, rgb_of, texts, third_line,
};
use super::glyph::ASCII_SELECTED;
use super::paint::{self, Colors, Shades};
use super::place::Tile;
use crate::app::App;
use crate::theme::{Bars, color_rgb};

/// A tile in colour: its body, a header row with its name when it has room, its size and
/// share below (on the header of a tile showing its children), and the selection's edge.
pub(super) fn fill_tile(
    buf: &mut Buffer,
    app: &App,
    tiles: &[Tile],
    i: usize,
    sel: Option<usize>,
    colors: &mut Colors,
    today: i64,
) {
    let t = &app.theme;
    let tile = &tiles[i];
    let r = cells_of(buf, tile);
    if r.is_empty() {
        return;
    }
    let base = rgb_of(base_style(app, tiles, i, today).0);
    let (name, size) = texts(app, tile);
    if tile.small > 0 {
        // The merged tile: shaded in its parent's colour, labelled only when that fits.
        let style = Style::new().fg(colors.get(base));
        let shade = paint::shade(t.bars, 3).repeat(usize::from(r.width));
        for y in r.top()..r.bottom() {
            buf.set_stringn(r.x, y, &shade, usize::from(r.width), style);
        }
        if r.height >= LABEL_MIN.1 && Span::raw(&name).width() + 2 <= usize::from(r.width) {
            put(buf, r, r.y, &format!("{name} "), t.fg);
        }
        return;
    }
    let selected = tile.level == 0 && sel == Some(tile.top) && tile.entry.is_some();
    let accent = t.border_focus.fg.and_then(color_rgb).unwrap_or(base);
    let shades = Shades::of(t);
    let body = shades.body(base, tile.level);
    let body_style = colors.on(body);
    for y in r.top()..r.bottom() {
        buf.set_stringn(
            r.x,
            y,
            " ".repeat(usize::from(r.width)),
            usize::from(r.width),
            body_style,
        );
    }
    let labelled = r.width >= LABEL_MIN.0 && r.height >= LABEL_MIN.1;
    let mut first_free = r.y;
    if labelled {
        let head = if selected {
            accent
        } else {
            shades.header(base)
        };
        let head_style = colors.on(head);
        let row = " ".repeat(usize::from(r.width));
        buf.set_stringn(r.x, r.y, row, usize::from(r.width), head_style);
        let room = usize::from(r.width) - 1;
        if let Some(name) = fit_name(&name, room) {
            let one_line = format!("{name}  {}", size.both);
            let text = if tile.nested && Span::raw(&one_line).width() < room {
                one_line
            } else {
                name
            };
            put(buf, r, r.y, &text, head_style.add_modifier(Modifier::BOLD));
        }
        first_free = r.y + 1;
        if !tile.nested {
            put(buf, r, r.y + 1, size.fit(room), body_style);
            first_free = r.y + 2;
            if r.height >= 3
                && let Some(text) = third_line(app, tile, today)
                && Span::raw(&text).width() < room
            {
                put(
                    buf,
                    r,
                    r.y + 2,
                    &text,
                    body_style.add_modifier(Modifier::DIM),
                );
                first_free = r.y + 3;
            }
        }
    }
    if candidate(tile) {
        let style = Style::new()
            .fg(colors.get(shades.header(base)))
            .bg(colors.get(body));
        // Sparser than without colour: the fill already tells the tile apart.
        hatch(buf, r, first_free, style, t.bars, 4);
    }
    if selected {
        edge(
            buf,
            r,
            labelled,
            Style::new().fg(colors.get(accent)),
            t.bars,
        );
        if !labelled {
            let style = colors.on(accent);
            for y in r.top()..r.bottom() {
                for x in r.left()..r.right() {
                    buf[(x, y)].set_style(style);
                }
            }
        }
    }
}

/// The selected tile's edge in `style` over its fill: its sides and bottom (the header row,
/// when there is one, is the top).
fn edge(buf: &mut Buffer, r: Rect, header: bool, style: Style, bars: Bars) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let set = match bars {
        Bars::Unicode => border::THICK,
        Bars::Ascii => ASCII_SELECTED,
    };
    let (left, right, bottom) = (r.left(), r.right() - 1, r.bottom() - 1);
    let top = if header { r.top() + 1 } else { r.top() };
    for y in top..bottom {
        buf[(left, y)]
            .set_symbol(set.vertical_left)
            .set_style(style);
        buf[(right, y)]
            .set_symbol(set.vertical_right)
            .set_style(style);
    }
    for x in left + 1..right {
        buf[(x, bottom)]
            .set_symbol(set.horizontal_bottom)
            .set_style(style);
    }
    buf[(left, bottom)]
        .set_symbol(set.bottom_left)
        .set_style(style);
    buf[(right, bottom)]
        .set_symbol(set.bottom_right)
        .set_style(style);
    if !header {
        for x in left + 1..right {
            buf[(x, top)]
                .set_symbol(set.horizontal_top)
                .set_style(style);
        }
        buf[(left, top)].set_symbol(set.top_left).set_style(style);
        buf[(right, top)].set_symbol(set.top_right).set_style(style);
    }
}
