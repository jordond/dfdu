//! What a tile shows whichever way it is painted: the colour it takes (its category, age band
//! or top folder), its labels, and the hatching of cleanup candidates. Tiles under 8x2 cells
//! have no text, and a name that does not fit is dropped rather than cut to a stub.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use super::place::Tile;
use super::{ColorBy, color_shown};
use crate::app::App;
use crate::format::{count, human};
use crate::theme::{Bars, Category, Rgb, color_rgb};

/// Age bands for [`ColorBy::Age`]: their upper bound in days and name.
pub(super) const AGES: [(i64, &str); 4] = [
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

pub(super) fn age_band(mtime: i64, now: i64) -> usize {
    let days = now.saturating_sub(mtime) / 86_400;
    AGES.iter()
        .position(|&(d, _)| days < d)
        .unwrap_or(AGES.len() - 1)
}

pub(super) fn age_style(app: &App, band: usize) -> Style {
    let t = &app.theme;
    [t.status_ok, t.fg, t.status_warn, t.fg_dim][band]
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
        _ => tile.parent_size,
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
        Some(crate::format::truncate_middle(name, w))
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
pub(super) fn hatch_glyph(bars: Bars) -> &'static str {
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::features::treemap::tests::buffer_row;
    use crate::theme::Theme;

    const DAY: i64 = 86_400;

    #[test]
    fn ages_fall_in_their_band() {
        let now = 1_792_324_800;
        let bands: Vec<usize> = [3, 30, 200, 2_000]
            .iter()
            .map(|days| age_band(now - days * DAY, now))
            .collect();
        assert_eq!(bands, [0, 1, 2, 3]);
        // A time ahead of the clock is new.
        assert_eq!(age_band(now + DAY, now), 0);
    }

    #[test]
    fn labels_name_the_tile_and_its_share_of_the_one_it_is_in() {
        let app = App::new(Theme::no_color(), Instant::now());
        let tile = Tile {
            level: 1,
            is_dir: true,
            parent_size: 200,
            ..Tile::of(Rect::new(0, 0, 10, 4), "photos", 50)
        };
        let (name, sizes) = texts(&app, &tile);
        assert_eq!(name, "photos/");
        assert_eq!(sizes.both, "50 B  25%");
        assert_eq!(sizes.fit(20), "50 B  25%");
        assert_eq!(
            sizes.fit(7),
            "50 B",
            "a share that does not fit is left out"
        );
        assert_eq!(sizes.fit(4), "", "a size is not cut");
        let merged = Tile {
            small: 3,
            ..Tile::of(Rect::new(0, 0, 10, 4), "", 1000)
        };
        assert_eq!(texts(&app, &merged).0, "3 more · 1000 B");
        assert_eq!(third_line(&app, &merged, 0), None);
    }

    #[test]
    fn a_tile_beside_a_sibling_leaves_a_gutter() {
        let buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        let tile = Tile {
            bound: Rect::new(0, 0, 20, 5),
            ..Tile::of(Rect::new(0, 0, 12, 5), "a", 1)
        };
        assert_eq!(cells_of(&buf, &tile), Rect::new(0, 0, 11, 5));
        let last = Tile {
            bound: Rect::new(0, 0, 20, 5),
            ..Tile::of(Rect::new(12, 0, 8, 5), "b", 1)
        };
        assert_eq!(cells_of(&buf, &last), Rect::new(12, 0, 8, 5));
    }

    #[test]
    fn hatching_takes_one_cell_in_every_few_along_diagonals() {
        let r = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::empty(r);
        hatch(&mut buf, r, 1, Style::new(), Bars::Ascii, 2);
        let rows: Vec<String> = (0..3).map(|y| buffer_row(&buf, y)).collect();
        assert_eq!(rows, ["      ", " / / /", "/ / / "]);
        put(&mut buf, r, 0, "name", Style::new());
        assert_eq!(buf[(1, 0)].symbol(), "n", "a cell in");
    }

    #[test]
    fn names_are_whole_cut_in_the_middle_or_left_out() {
        assert_eq!(fit_name("dir-1/", 8).as_deref(), Some("dir-1/"));
        assert_eq!(fit_name("directory-one/", 8), None, "never a stub");
        let cut = fit_name("a-directory-with-a-long-name/", 14).expect("a cut name");
        assert!(cut.starts_with("a-dir") && cut.ends_with("name/"), "{cut}");
    }
}
