//! Drawing the map: the legend line (what the colours mean, and the depth), the tiles with
//! their labels, and the line about the selected tile.
//!
//! In colour ([`Paint::Fill`]) tiles are filled, with a gutter of one cell between siblings
//! side by side and no borders; a tile with room gets a header row with its name and a line
//! with its size and share. The selected tile alone is outlined: its header row and edge in the
//! focus colour. Without colour ([`Paint::Glyphs`]) tiles keep light frames, the selected one
//! heavy, and are filled with shade glyphs by depth. What a tile says and which colour it takes
//! is the same in both (`tile`).

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::fill::fill_tile;
use super::glyph::glyph_tile;
use super::paint::{Colors, Paint};
use super::place::{build, peek};
use super::tile::{AGES, age_style, base_style, hatch_glyph};
use super::{ColorBy, color_shown, key_of, selected_top};
use crate::app::App;
use crate::app::keys::{Action, first_key};
use crate::format::{count, human, percent};
use crate::theme::Category;
use crate::view::empty::{self, Empty};
use crate::view::spread;

/// Draws the map in `area`: the legend line, the tiles, and a line on the selected entry.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant) {
    crate::state::motion::frame(app, now, &[]);
    app.list_height = area.height as usize;
    let [legend, map, info] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(1),
    ])
    .areas(area);
    let paint = Paint::of(&app.theme);
    let framed = paint == Paint::Glyphs;
    let key = key_of(app);
    let below = &app.extras.treemap;
    let current = app.treemap.layout.as_ref().is_some_and(|l| {
        l.key == key
            && l.area == map
            && l.below == below.key
            && l.classes == below.classes
            && l.framed == framed
    });
    if !current {
        app.treemap.layout = Some(build(app, map, framed));
    }
    let today = dfdu_tree::now_secs();
    let app = &*app;
    if let Some(line) = selected_line(app) {
        // The selected tile's place among the top-level tiles on the right, as the list's
        // selection line has the row's.
        let at = position(app).map(|p| Span::styled(p, app.theme.fg_dim));
        let w = at.as_ref().map_or(0, |s| crate::view::cells(s.width()));
        let [left, right] = ratatui::layout::Layout::horizontal([
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(w),
        ])
        .areas(info);
        f.render_widget(Paragraph::new(line), left);
        if let Some(at) = at {
            f.render_widget(Paragraph::new(Line::from(at)), right);
        }
    }
    let Some(l) = &app.treemap.layout else {
        return;
    };
    let sel = selected_top(app);
    // At depth 1 the selected tile shows what is in it, in place of its tile.
    let peeked = sel
        .filter(|_| app.treemap.depth == 1)
        .map(|s| (l.tops[s], peek(app, l, l.tops[s])))
        .filter(|(_, tiles)| !tiles.is_empty());
    let mut slots = Vec::new();
    for tiles in [
        &l.tiles[..],
        peeked.as_ref().map_or(&[][..], |(_, p)| &p[..]),
    ] {
        for i in 0..tiles.len() {
            if let Some(slot) = base_style(app, tiles, i, today).1
                && !slots.contains(&slot)
            {
                slots.push(slot);
            }
        }
    }
    draw_legend(f, app, legend, &slots);
    if l.tiles.is_empty() {
        let e = Empty {
            title: "Map",
            text: vec![
                "Shows this directory as tiles sized by what they hold; inside the selected \
                 tile, what Enter opens."
                    .into(),
                if app.session.filter.active() {
                    "Nothing here matches the filter.".into()
                } else {
                    "This directory is empty.".into()
                },
            ],
            keys: vec![
                // The map takes the arrows to move between tiles.
                ("Backspace", "parent directory"),
                (first_key(Action::Close), "back to Files"),
            ],
        };
        empty::draw(f, &app.theme, map, &e);
        return;
    }
    let mut colors = Colors::new(app.theme.level());
    let buf = f.buffer_mut();
    let skip = peeked.as_ref().map(|(at, _)| *at);
    let mut layers = vec![(&l.tiles[..], skip)];
    if let Some((_, p)) = &peeked {
        layers.push((&p[..], None));
    }
    for (tiles, skip) in layers {
        for i in (0..tiles.len()).filter(|&i| Some(i) != skip) {
            let skipped_parent = skip.is_some_and(|s| tiles[i].up == Some(s));
            if skipped_parent {
                continue;
            }
            match paint {
                Paint::Fill(_) => fill_tile(buf, app, tiles, i, sel, &mut colors, today),
                Paint::Glyphs => glyph_tile(buf, app, tiles, i, sel, today),
            }
        }
    }
}

/// The colour slot names of the legend, as the theme's category tokens are named.
fn slot_name(slot: usize) -> &'static str {
    match Category::ALL.get(slot) {
        Some(Category::Cache) => "cache",
        Some(Category::Build) => "build",
        Some(Category::Vm) => "vm",
        Some(Category::Toolchain) => "toolchain",
        Some(Category::Media) => "media",
        Some(Category::Download) => "download",
        Some(Category::Container) => "container",
        Some(Category::Other) | None => "other",
    }
}

/// The legend line: the colouring and what its colours mean (the categories on screen, or the
/// age bands), the hatching of cleanup candidates, and the depth with its keys on the right.
fn draw_legend(f: &mut Frame, app: &App, area: Rect, slots: &[usize]) {
    let t = &app.theme;
    let shown = color_shown(app);
    let mut left = vec![Span::styled(
        format!(" colour: {}", shown.label()),
        t.fg_dim,
    )];
    let mut key = |style: Style, name: &str| {
        left.push(Span::raw("  "));
        left.push(Span::styled("■", style));
        left.push(Span::styled(format!(" {name}"), t.fg_dim));
    };
    match shown {
        ColorBy::Category => {
            for &slot in slots {
                key(t.cat[slot], slot_name(slot));
            }
        }
        ColorBy::Age => {
            for (band, (_, name)) in AGES.iter().enumerate() {
                key(age_style(app, band), name);
            }
        }
        ColorBy::Folder => {}
    }
    let hatch = hatch_glyph(t.bars);
    left.push(Span::styled(
        format!("  {hatch} cleanup candidate"),
        t.fg_dim,
    ));
    let right = vec![Span::styled(
        format!(
            "depth {}  {} {} ",
            app.treemap.depth,
            first_key(Action::TreemapDeeper),
            first_key(Action::TreemapShallower)
        ),
        t.fg_dim,
    )];
    f.render_widget(Paragraph::new(spread(left, right, area.width)), area);
}

/// ` 3/12 `: the place of the selected tile among the top-level tiles, in the order they were
/// laid out (largest first). `None` when the selection has no tile of its own.
fn position(app: &App) -> Option<String> {
    let tops = app.treemap.layout.as_ref()?.tops.len();
    selected_top(app).map(|i| format!(" {}/{tops} ", i + 1))
}

/// The line under the map: the selected tile's name, size, share and items.
fn selected_line(app: &App) -> Option<Line<'static>> {
    let e = app.session.selected_entry()?;
    let total = app.session.size_mode.of(&app.session.snap.dir_total).max(1);
    let mut text = format!(
        " ▶ {}{}  {}  {:.1}%",
        String::from_utf8_lossy(&e.name),
        if e.is_dir { "/" } else { "" },
        human(e.size),
        percent(e.size, total)
    );
    if e.is_dir {
        text.push_str("  ·  ");
        text.push_str(&count(e.items));
        text.push_str(" items");
    }
    Some(Line::styled(text, app.theme.fg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legend_names_follow_the_category_tokens() {
        assert_eq!(slot_name(Category::Build as usize), "build");
        assert_eq!(slot_name(Category::Other as usize), "other");
    }
}
