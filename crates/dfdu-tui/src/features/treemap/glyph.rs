//! A tile without colour (`Paint::Glyphs`): a light frame, heavy when selected, filled with
//! the shade glyph of its depth.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Widget};

use super::tile::{
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::model::snapshot::test_entry;
    use crate::theme::Theme;

    const AREA: Rect = Rect::new(0, 0, 16, 6);

    /// Tile 0 of `tiles` drawn into a buffer of [`AREA`], selected when `selected`.
    fn drawn(theme: Theme, tiles: &[Tile], selected: bool) -> Buffer {
        let app = App::new(theme, Instant::now());
        let mut buf = Buffer::empty(AREA);
        glyph_tile(&mut buf, &app, tiles, 0, selected.then_some(0), 0);
        buf
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (AREA.left()..AREA.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn a_tile_is_framed_labelled_and_shaded_by_depth() {
        let tiles = [Tile::of(AREA, "notes", 300)];
        let buf = drawn(Theme::no_color(), &tiles, false);
        assert_eq!(row(&buf, 0), format!("┌{}┐", "─".repeat(14)));
        assert!(row(&buf, 1).starts_with("│ notes "), "{:?}", row(&buf, 1));
        assert!(row(&buf, 2).starts_with("│ 300 B "), "{:?}", row(&buf, 2));
        let shade = paint::shade(Bars::Unicode, 0);
        assert_eq!(row(&buf, 3), format!("│{}│", shade.repeat(14)));
    }

    #[test]
    fn the_selected_tile_has_a_heavy_frame_and_ascii_has_its_own() {
        let node = test_entry(0, "notes", 300).node;
        let tiles = [Tile {
            entry: Some((node, 0)),
            ..Tile::of(AREA, "notes", 300)
        }];
        let buf = drawn(Theme::no_color(), &tiles, true);
        assert_eq!(buf[(0, 0)].symbol(), "┏");
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        let mut ascii = Theme::no_color();
        ascii.bars = Bars::Ascii;
        assert_eq!(drawn(ascii.clone(), &tiles, true)[(0, 0)].symbol(), "#");
        assert_eq!(drawn(ascii, &tiles, false)[(0, 0)].symbol(), "+");
    }

    #[test]
    fn slivers_are_shaded_only_when_wide_enough_or_selected() {
        let narrow = |w| [Tile::of(Rect::new(0, 0, w, 6), "s", 1)];
        let shade = paint::shade(Bars::Unicode, 0);
        assert_eq!(drawn(Theme::no_color(), &narrow(3), false)[(1, 2)].symbol(), shade);
        assert_eq!(drawn(Theme::no_color(), &narrow(2), false)[(1, 2)].symbol(), " ");
        let node = test_entry(0, "s", 1).node;
        let selected = [Tile {
            entry: Some((node, 0)),
            ..narrow(2)[0].clone()
        }];
        assert_eq!(drawn(Theme::no_color(), &selected, true)[(1, 2)].symbol(), shade);
    }
}
