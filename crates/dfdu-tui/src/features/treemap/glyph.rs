//! A tile without colour (`Paint::Glyphs`): a light frame, heavy when selected, filled with
//! the shade glyph of its depth.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Widget};

use super::draw::{
    LABEL_MIN, SLIVER, base_style, candidate, fit_name, hatch, put, texts, third_line,
};
use super::paint;
use super::place::Tile;
use crate::app::App;
use crate::theme::Bars;

/// A tile's frame with [`Bars::Ascii`]: `=` across, `|` down, `+` at the corners.
const ASCII_FRAME: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "=",
    horizontal_bottom: "=",
};

/// The selected tile's frame with [`Bars::Ascii`], all `#`.
pub(super) const ASCII_SELECTED: border::Set = border::Set {
    top_left: "#",
    top_right: "#",
    bottom_left: "#",
    bottom_right: "#",
    vertical_left: "#",
    vertical_right: "#",
    horizontal_top: "#",
    horizontal_bottom: "#",
};

/// Fills the rows of `r` from `from` down with `glyph` in `style`.
fn shade_rows(buf: &mut Buffer, r: Rect, from: u16, glyph: &str, style: Style) {
    let row = glyph.repeat(usize::from(r.width));
    for y in from.max(r.top())..r.bottom() {
        buf.set_stringn(r.x, y, &row, usize::from(r.width), style);
    }
}

/// A tile without colour: a light frame (heavy when selected) filled with the shade of its
/// depth, its labels on cleared rows at the top; a tile too small for a frame is shade only.
pub(super) fn glyph_tile(
    buf: &mut Buffer,
    app: &App,
    tiles: &[Tile],
    i: usize,
    sel: Option<usize>,
    today: i64,
) {
    let t = &app.theme;
    let tile = &tiles[i];
    let r = tile.rect.intersection(buf.area);
    if r.is_empty() {
        return;
    }
    let style = base_style(app, tiles, i, today).0;
    let selected = tile.level == 0 && sel == Some(tile.top) && tile.entry.is_some();
    let (name, size) = texts(app, tile);
    let glyph = if tile.small > 0 {
        paint::shade(t.bars, 3)
    } else {
        paint::shade(t.bars, tile.level)
    };
    let fill_style = if tile.small > 0 { t.fg_dim } else { style };
    if r.width < 4 || r.height < 3 {
        // Slivers are left blank unless selected, so the frames beside them stay clean.
        if selected {
            shade_rows(buf, r, r.y, glyph, t.cursor);
        } else if r.width >= SLIVER {
            shade_rows(buf, r, r.y, glyph, fill_style);
        }
        return;
    }
    let (border, border_type) = if selected {
        let focus = t.border_focus.add_modifier(Modifier::BOLD);
        (focus, BorderType::Thick)
    } else {
        (style, BorderType::Plain)
    };
    let mut block = Block::bordered()
        .border_type(border_type)
        .border_style(border);
    if t.bars == Bars::Ascii {
        block = block.border_set(if selected {
            ASCII_SELECTED
        } else {
            ASCII_FRAME
        });
    }
    block.render(r, buf);
    let inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
    let w = usize::from(inner.width);
    let name_style = if tile.is_dir {
        t.dir.patch(style)
    } else {
        style
    };
    // The label lines: the name, then the size and the third line, as many as fit and say
    // something; a tile showing its children has one, below which they go.
    let mut lines: Vec<(String, Style)> = Vec::new();
    if r.width >= LABEL_MIN.0
        && let Some(n) = fit_name(&name, w.saturating_sub(1))
    {
        let one_line = format!("{n}  {}", size.both);
        if (tile.nested || inner.height == 1) && Span::raw(&one_line).width() < w {
            lines.push((one_line, name_style));
        } else {
            lines.push((n, name_style));
            let size = size.fit(w);
            if !tile.nested && !size.is_empty() {
                lines.push((size.to_owned(), t.fg));
            }
            if let Some(text) = third_line(app, tile, today).filter(|s| Span::raw(s).width() < w) {
                lines.push((text, t.fg_dim));
            }
        }
    }
    let rows = if tile.nested {
        1
    } else {
        lines.len().min(usize::from(inner.height))
    };
    let rows = crate::view::cells(rows);
    for (y, (text, style)) in (inner.y..inner.y + rows).zip(&lines) {
        buf.set_stringn(inner.x, y, " ".repeat(w), w, Style::new());
        put(buf, inner, y, text, *style);
    }
    if tile.nested {
        return;
    }
    let below = inner.y + rows;
    if candidate(tile) {
        shade_rows(buf, inner, below, " ", Style::new());
        hatch(buf, inner, below, style, t.bars, 2);
    } else {
        shade_rows(buf, inner, below, glyph, fill_style);
    }
}
