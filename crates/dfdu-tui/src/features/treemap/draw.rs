//! Drawing the treemap: the tiles with their labels, the legend of the colouring, and the line
//! about the selected tile.

use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};

use super::{ColorBy, Tile, build, key_of, selected_top};
use crate::app::App;
use crate::app::keys::{self, Action};
use crate::theme::{count, human, percent};

/// Age bands for [`ColorBy::Age`]: their upper bound in days and name.
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
    [t.status_ok, t.fg, t.status_warn, t.fg_dim][band]
}

/// Draws the treemap in `area`: a legend line, the tiles, and a line on the selected entry.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, now: Instant) {
    crate::state::motion::frame(app, now, &[]);
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
    let today = dfdu_tree::now_secs();
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
            Paragraph::new(Line::styled("  (empty)", app.theme.fg_dim)),
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
        format!(" colour: {}", app.treemap.color.label()),
        t.fg_dim,
    )];
    if app.treemap.color == ColorBy::Category {
        spans.push(Span::styled("  ╱ cleanup candidate", t.fg_dim));
    }
    if app.treemap.color == ColorBy::Age {
        for (band, (_, name)) in AGES.iter().enumerate() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("■", age_style(app, band)));
            spans.push(Span::styled(format!(" {name}"), t.fg_dim));
        }
    }
    spans.push(Span::styled(
        format!(
            "   {} colour  depth {} ({} {})",
            keys::first_key(Action::TreemapColor),
            app.treemap.depth,
            keys::first_key(Action::TreemapDeeper),
            keys::first_key(Action::TreemapShallower),
        ),
        t.fg_dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn tile_style(app: &App, tile: &Tile, today: i64) -> Style {
    let t = &app.theme;
    if tile.small > 0 {
        return t.fg_dim;
    }
    match app.treemap.color {
        ColorBy::Folder => t.cat[tile.top % t.cat.len()],
        ColorBy::Age => age_style(app, age_band(tile.mtime, today)),
        ColorBy::Category => match (&app.snap.rules, tile.tag) {
            (Some(rules), Some(tag)) => t.cat[tag.category(rules).1],
            _ => t.fg_dim,
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

/// Hatches `inner` below its label lines (name, size, category), in `style`.
fn hatch(buf: &mut Buffer, inner: Rect, style: Style) {
    for y in (inner.top() + 3).min(inner.bottom())..inner.bottom() {
        for x in inner.left()..inner.right() {
            if (x + y) % 2 == 0 {
                buf[(x, y)].set_symbol("╱").set_style(style);
            }
        }
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
    let name = format!(
        "{}{}{}",
        if marked { "✓ " } else { "" },
        tile.name,
        if tile.is_dir { "/" } else { "" }
    );
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
        let (border, border_type) = if selected {
            let focus = app.theme.border_focus.add_modifier(Modifier::BOLD);
            (focus, BorderType::Thick)
        } else {
            (style, BorderType::Plain)
        };
        Block::bordered()
            .border_type(border_type)
            .border_style(border)
            .render(r, buf);
        let mut inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
        // Cleanup candidates and what is in them are hatched, under the labels; a framed
        // tile's own tiles are.
        let candidate = tile.tag.is_some_and(crate::model::snapshot::Tag::candidate);
        if app.treemap.color == ColorBy::Category && candidate && !tile.framed {
            hatch(buf, inner, style);
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
            if app.treemap.color == ColorBy::Age && tile.small == 0 && inner.height >= 3 {
                let band = AGES[age_band(tile.mtime, today)].1;
                buf.set_stringn(
                    inner.x,
                    inner.y + 2,
                    format!("≤ {band}"),
                    w,
                    app.theme.fg_dim,
                );
            }
            if app.treemap.color == ColorBy::Category
                && inner.height >= 3
                && let (Some(rules), Some(tag)) = (&app.snap.rules, tile.tag)
                && !tag.inherited
            {
                let safety = tag.class.safety.as_str();
                let text = format!("{} · {safety}", tag.category(rules).0);
                buf.set_stringn(inner.x, inner.y + 2, text, w, app.theme.fg_dim);
            }
        }
    } else {
        // Too small for a frame: a shaded block with what fits of the name.
        let fill = if selected { app.theme.cursor } else { style };
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
        percent(e.size, total)
    );
    if e.is_dir {
        text.push_str("  ·  ");
        text.push_str(&count(e.items));
        text.push_str(" items");
    }
    Some(Line::styled(text, app.theme.fg))
}
