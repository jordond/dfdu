//! What a tile shows whichever way it is painted: the colour it takes (its category, age band
//! or top folder), its labels, and the hatching of cleanup candidates. Tiles under 8x2 cells
//! have no text, and a name that does not fit is dropped rather than cut to a stub.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use dfdu_session::treemap as map;

use super::place::{Tile, rect};
use super::{ColorBy, color_shown};
use crate::app::App;
use crate::format::{count, human};
use crate::theme::{Bars, Rgb, color_rgb};

pub(super) use dfdu_session::treemap::AGES;

/// A tile needs this many cells across and down for any text.
pub(super) const LABEL_MIN: (u16, u16) = (8, 2);
/// A name cut in the middle keeps at least this many cells, or it is left out.
const CUT_NAME_MIN: usize = 12;
/// Tiles narrower than this get no fill without colour.
pub(super) const SLIVER: u16 = 3;
/// The colour of a tile whose base colour has no value (`Reset`).
const GREY: Rgb = (0x80, 0x80, 0x80);

/// The style of age band `band` ([`dfdu_session::treemap::age_token`]).
pub(super) fn age_style(app: &App, band: usize) -> Style {
    app.theme.style(map::age_token(band))
}

/// The style a tile's colour comes from, and its category colour slot when it is coloured by
/// one ([`dfdu_session::treemap::base`]).
pub(super) fn base_style(
    app: &App,
    tiles: &[Tile],
    i: usize,
    today: i64,
) -> (Style, Option<usize>) {
    let (token, slot) = map::base(&app.session, tiles, i, today);
    (app.theme.style(token), slot)
}

/// What a tile's labels say: its name (with `✓ ` when marked and `/` for a directory), and
/// its size with its share of the directory it is in.
pub(super) fn texts(app: &App, tile: &Tile) -> (String, Sizes) {
    if tile.small > 0 {
        let text = format!("{} more · {}", count(tile.small), human(tile.size));
        return (text, Sizes::default());
    }
    let marked = tile
        .entry
        .is_some_and(|(node, _)| app.session.marks.contains(node));
    let name = format!(
        "{}{}{}",
        if marked { "✓ " } else { "" },
        tile.name,
        if tile.is_dir { "/" } else { "" }
    );
    let size = tile.live_size(&app.session);
    // Top-level tiles show live sizes, so their share is of the directory's live total.
    let share = (size.saturating_mul(100))
        .checked_div(tile.whole(&app.session))
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
        ColorBy::Age => Some(format!("≤ {}", AGES[map::age_band(tile.mtime, today)].1)),
        ColorBy::Category => {
            let (rules, tag) = (app.session.snap.rules.as_ref()?, tile.tag?);
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
    tile.candidate()
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
    let mut r = rect(tile.rect).intersection(buf.area);
    if rect(tile.rect).right() < rect(tile.bound).right() && r.width > 1 {
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
    use crate::features::treemap::layout::area_of;
    use crate::features::treemap::place::tile_of;
    use crate::features::treemap::tests::buffer_row;
    use crate::theme::Theme;

    #[test]
    fn labels_name_the_tile_and_its_share_of_the_one_it_is_in() {
        let app = App::new(Theme::no_color(), Instant::now());
        let tile = Tile {
            level: 1,
            is_dir: true,
            parent_size: 200,
            ..tile_of(Rect::new(0, 0, 10, 4), "photos", 50)
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
            ..tile_of(Rect::new(0, 0, 10, 4), "", 1000)
        };
        assert_eq!(texts(&app, &merged).0, "3 more · 1000 B");
        assert_eq!(third_line(&app, &merged, 0), None);
    }

    #[test]
    fn a_tile_beside_a_sibling_leaves_a_gutter() {
        let buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        let tile = Tile {
            bound: area_of(Rect::new(0, 0, 20, 5)),
            ..tile_of(Rect::new(0, 0, 12, 5), "a", 1)
        };
        assert_eq!(cells_of(&buf, &tile), Rect::new(0, 0, 11, 5));
        let last = Tile {
            bound: area_of(Rect::new(0, 0, 20, 5)),
            ..tile_of(Rect::new(12, 0, 8, 5), "b", 1)
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
