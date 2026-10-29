//! Drawing the map: the legend line (what the colours mean, and the depth), the tiles with
//! their labels, and the line about the selected tile.
//!
//! In colour ([`Paint::Fill`]) tiles are filled, with a gutter of one cell between siblings
//! side by side and no borders; a tile with room gets a header row with its name and a line
//! with its size and share. The selected tile alone is outlined: its header row and edge in the
//! focus colour. Without colour ([`Paint::Glyphs`]) tiles keep light frames, the selected one
//! heavy, and are filled with shade glyphs by depth. In both, tiles under 8x2 cells have no
//! text, and a name that does not fit is dropped rather than cut to a stub. Cleanup candidates
//! are hatched in every colouring.

use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::fill::fill_tile;
use super::glyph::glyph_tile;
use super::paint::{Colors, Paint};
use super::place::{Tile, build, peek};
use super::{ColorBy, color_shown, key_of, selected_top};
use crate::app::App;
use crate::app::keys::{Action, first_key};
use crate::theme::{Bars, Category, Rgb, color_rgb, count, human, percent};
use crate::view::spread;

/// Age bands for [`ColorBy::Age`]: their upper bound in days and name.
const AGES: [(i64, &str); 4] = [
    (7, "week"),
    (90, "3 months"),
    (365, "year"),
    (i64::MAX, "older"),
];

/// A tile needs this many cells across and down for any text.
pub(super) const LABEL_MIN: (u16, u16) = (8, 2);
/// A name cut in the middle keeps at least this many cells, or it is left out.
const CUT_NAME_MIN: usize = 12;
/// Tiles narrower than this get no fill without colour.
pub(super) const SLIVER: u16 = 3;
/// The colour of a tile whose base colour has no value (`Reset`).
const GREY: Rgb = (0x80, 0x80, 0x80);

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
    let current = app.treemap.layout.as_ref().is_some_and(|l| {
        l.key == key && l.area == map && l.below == app.extras.treemap.key && l.framed == framed
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
        f.render_widget(
            Paragraph::new(Line::styled("  (empty)", app.theme.fg_dim)),
            map,
        );
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

/// The style a tile's colour comes from, and its category colour slot when it is coloured by
/// one. In category colouring a tile takes its match's category, a directory without one the
/// category most of it is in, and anything else its parent's colour; by top folder every tile
/// takes its top-level tile's colour.
pub(super) fn base_style(
    app: &App,
    tiles: &[Tile],
    i: usize,
    today: i64,
) -> (Style, Option<usize>) {
    let t = &app.theme;
    let tile = &tiles[i];
    let parent = |app: &App| tile.up.map(|u| base_style(app, tiles, u, today));
    if tile.small > 0 {
        return parent(app).unwrap_or((t.fg_dim, None));
    }
    match color_shown(app) {
        ColorBy::Folder => parent(app).unwrap_or((t.cat[tile.top % t.cat.len()], None)),
        ColorBy::Age => (age_style(app, age_band(tile.mtime, today)), None),
        ColorBy::Category => {
            let slot = match (&app.snap.rules, tile.tag) {
                (Some(rules), Some(tag)) => Some(tag.category(rules).1),
                _ => tile.dominant,
            };
            match slot {
                Some(s) => (t.cat[s], Some(s)),
                None => parent(app).unwrap_or((t.cat[Category::Other as usize], None)),
            }
        }
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

/// What a tile's labels say: its name (with `✓ ` when marked and `/` for a directory), and
/// its size with its share of the directory it is in.
pub(super) fn texts(app: &App, tile: &Tile) -> (String, Sizes) {
    if tile.small > 0 {
        let text = format!("{} more · {}", count(tile.small), human(tile.size));
        return (text, Sizes::default());
    }
    let marked = tile.entry.is_some_and(|(node, _)| app.marks.contains(node));
    let name = format!(
        "{}{}{}",
        if marked { "✓ " } else { "" },
        tile.name,
        if tile.is_dir { "/" } else { "" }
    );
    let size = live_size(app, tile);
    // Top-level tiles show live sizes, so their share is of the directory's live total.
    let parent = match tile.level {
        0 => app.size_mode.of(&app.snap.dir_total),
        _ => tile.parent,
    };
    let share = (size.saturating_mul(100))
        .checked_div(parent)
        .map_or(String::new(), |p| format!("  {p}%"));
    let size = human(size);
    (
        name,
        Sizes {
            both: format!("{size}{share}"),
            size,
        },
    )
}

/// A tile's size with its share, and alone for when the share does not fit with it: a cut
/// size would read as another number.
#[derive(Default)]
pub(super) struct Sizes {
    pub(super) both: String,
    size: String,
}

impl Sizes {
    /// The longest form shorter than `room` cells; empty when none is.
    pub(super) fn fit(&self, room: usize) -> &str {
        [&self.both, &self.size]
            .into_iter()
            .find(|s| Span::raw(s.as_str()).width() < room)
            .map_or("", String::as_str)
    }
}

/// `name` in `w` cells: whole, or cut in the middle when that leaves at least
/// [`CUT_NAME_MIN`] cells; `None` (no label) otherwise, never a stub like `dir-1…`.
pub(super) fn fit_name(name: &str, w: usize) -> Option<String> {
    if Span::raw(name).width() <= w {
        Some(name.to_owned())
    } else if w >= CUT_NAME_MIN {
        Some(crate::theme::truncate_middle(name, w))
    } else {
        None
    }
}

/// The third line of a leaf tile: its age band, or its own match's category and safety.
pub(super) fn third_line(app: &App, tile: &Tile, today: i64) -> Option<String> {
    if tile.small > 0 {
        return None;
    }
    match color_shown(app) {
        ColorBy::Age => Some(format!("≤ {}", AGES[age_band(tile.mtime, today)].1)),
        ColorBy::Category => {
            let (rules, tag) = (app.snap.rules.as_ref()?, tile.tag?);
            (!tag.inherited).then(|| {
                let safety = tag.class.safety.as_str();
                format!("{} · {safety}", tag.category(rules).0)
            })
        }
        ColorBy::Folder => None,
    }
}

/// The glyph that hatches cleanup candidates.
fn hatch_glyph(bars: Bars) -> &'static str {
    match bars {
        Bars::Unicode => "╱",
        Bars::Ascii => "/",
    }
}

/// Whether the tile is a cleanup candidate, hatched below its labels.
pub(super) fn candidate(tile: &Tile) -> bool {
    !tile.nested && tile.tag.is_some_and(crate::model::snapshot::Tag::candidate)
}

/// Hatches the rows of `r` from `from` down in `style`, one cell in `every` along diagonals.
pub(super) fn hatch(buf: &mut Buffer, r: Rect, from: u16, style: Style, bars: Bars, every: u16) {
    let symbol = hatch_glyph(bars);
    for y in from.max(r.top())..r.bottom() {
        for x in r.left()..r.right() {
            if (x + y) % every == 0 {
                buf[(x, y)].set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// The cells a tile fills: its rectangle on screen, less a gutter on the right when a sibling
/// is beside it.
pub(super) fn cells_of(buf: &Buffer, tile: &Tile) -> Rect {
    let mut r = tile.rect.intersection(buf.area);
    if tile.rect.right() < tile.bound.right() && r.width > 1 {
        r.width -= 1;
    }
    r
}

/// Writes `text` at the start of row `y` of `r`, a cell in, in `style`.
pub(super) fn put(buf: &mut Buffer, r: Rect, y: u16, text: &str, style: Style) {
    if r.width > 1 {
        buf.set_stringn(r.x + 1, y, text, usize::from(r.width - 1), style);
    }
}

pub(super) fn rgb_of(style: Style) -> Rgb {
    style.fg.and_then(color_rgb).unwrap_or(GREY)
}

/// ` 3/12 `: the place of the selected tile among the top-level tiles, in the order they were
/// laid out (largest first). `None` when the selection has no tile of its own.
fn position(app: &App) -> Option<String> {
    let tops = app.treemap.layout.as_ref()?.tops.len();
    selected_top(app).map(|i| format!(" {}/{tops} ", i + 1))
}

/// The line under the map: the selected tile's name, size, share and items.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_whole_cut_in_the_middle_or_left_out() {
        assert_eq!(fit_name("dir-1/", 8).as_deref(), Some("dir-1/"));
        assert_eq!(fit_name("directory-one/", 8), None, "never a stub");
        let cut = fit_name("a-directory-with-a-long-name/", 14).expect("a cut name");
        assert!(cut.starts_with("a-dir") && cut.ends_with("name/"), "{cut}");
    }

    #[test]
    fn legend_names_follow_the_category_tokens() {
        assert_eq!(slot_name(Category::Build as usize), "build");
        assert_eq!(slot_name(Category::Other as usize), "other");
    }
}
