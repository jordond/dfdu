//! Painting the tiles: each filled with a tint of the colour its colouring gives it (its
//! category, age band or top folder, `dfdu_session::treemap::base`), deeper tiles more strongly
//! tinted, a tile with room for a label under a header row with its name, and its size below;
//! cleanup candidates hatched. The selected tile has its header in the focus colour and an
//! edge; the one under the pointer a thin edge. Labels take black or white, whichever reads
//! better on their fill.

use std::sync::Arc;

use dfdu_session::SessionState;
use dfdu_session::format::{count, human};
use dfdu_session::palette::{Category, Token};
use dfdu_session::treemap::{self as map, AGES, ColorBy, Tile, TileTree};
use egui::text::{Galley, LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::{PAD, rect_of};
use crate::theme::Colors;

/// Half the seam between neighbouring tiles.
const SEAM: f32 = 0.5;
/// Rounding of a tile's corners.
const RADIUS: u8 = 2;
/// Room between the lines of hatching.
const HATCH: f32 = 7.0;
/// How much of the ground a body takes on a dark background, by depth: deeper tiles show more
/// of their hue.
const DARK_BODY: [f32; 4] = [0.74, 0.68, 0.62, 0.56];
/// How much of the ground a body takes on a light background, by depth.
const LIGHT_BODY: [f32; 4] = [0.6, 0.5, 0.4, 0.3];
/// How much of the ground a header row takes on a dark background, and how far it goes
/// towards black on a light one: the hue at its strongest.
const DARK_HEADER: f32 = 0.56;
const LIGHT_HEADER: f32 = 0.15;
/// A name is cut with an ellipsis only where this many text rows fit across; otherwise it is
/// left out rather than cut to a stub.
const CUT_NAME_MIN: f32 = 4.0;

/// `a` moved the fraction `t` of the way to `b`, channel by channel.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a mix of two channels stays within 0..=255"
)]
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// The label colour on `fill`: black or white, whichever contrasts more (WCAG).
fn ink(fill: Color32) -> Color32 {
    let c = egui::Rgba::from(fill);
    let l = 0.2126 * c.r() + 0.7152 * c.g() + 0.0722 * c.b();
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.05 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// The tints of the theme shown: dark ones on a dark background, light ones on a light one.
#[derive(Copy, Clone, Debug)]
struct Shades {
    dark: bool,
    ground: Color32,
}

impl Shades {
    fn header(self, base: Color32) -> Color32 {
        if self.dark {
            mix(base, self.ground, DARK_HEADER)
        } else {
            mix(base, Color32::BLACK, LIGHT_HEADER)
        }
    }

    fn body(self, base: Color32, level: u8) -> Color32 {
        let by = if self.dark { DARK_BODY } else { LIGHT_BODY };
        mix(base, self.ground, by[usize::from(level).min(by.len() - 1)])
    }
}

/// What paints the tiles of one frame.
pub struct Painter<'a> {
    s: &'a SessionState,
    colors: &'a Colors,
    /// The selected top-level tile.
    sel: Option<usize>,
    shades: Shades,
    font: FontId,
    line: f32,
    today: i64,
}

impl<'a> Painter<'a> {
    pub fn new(ui: &Ui, s: &'a SessionState, colors: &'a Colors, sel: Option<usize>) -> Self {
        let v = ui.visuals();
        Painter {
            s,
            colors,
            sel,
            shades: Shades {
                dark: v.dark_mode,
                ground: v.panel_fill,
            },
            font: egui::TextStyle::Body.resolve(ui.style()),
            line: ui.text_style_height(&egui::TextStyle::Body),
            today: dfdu_tree::now_secs(),
        }
    }

    /// The token tile `i` of `tiles` takes its colour from, and its category colour slot.
    pub fn base(&self, tiles: &[Tile], i: usize) -> (Token, Option<usize>) {
        map::base(self.s, tiles, i, self.today)
    }

    /// `text` in `color`, as one line no wider than `room`: whole, or when `cut` and there is
    /// room for it, cut with an ellipsis; `None` when it does not fit.
    fn line_of(
        &self,
        ui: &Ui,
        text: String,
        color: Color32,
        room: f32,
        cut: bool,
    ) -> Option<Arc<Galley>> {
        let mut job = LayoutJob::single_section(text, TextFormat::simple(self.font.clone(), color));
        let cut = cut && room >= self.line * CUT_NAME_MIN;
        if cut {
            job.wrap = TextWrapping::truncate_at_width(room);
        }
        let g = ui.painter().layout_job(job);
        // A cut line fits by its cut; the tile's clip keeps the ellipsis inside.
        (cut || g.size().x <= room).then_some(g)
    }

    /// Paints tile `i` of `tiles`.
    pub fn tile(&self, ui: &Ui, tiles: &[Tile], i: usize) {
        let tile = &tiles[i];
        let r = rect_of(tile.rect).shrink(SEAM);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
        let base = self.colors_of(tiles, i);
        let room = r.width() - 2.0 * PAD;
        let at = r.min + vec2(PAD, PAD);
        if tile.small > 0 {
            let fill = self.shades.body(base, 3);
            p.rect_filled(r, RADIUS, fill);
            let text = format!("{} more · {}", count(tile.small), human(tile.size));
            if tile.room.label
                && let Some(g) = self.line_of(ui, text, ink(fill), room, false)
            {
                p.galley(at, g, ink(fill));
            }
            return;
        }
        let top_level = tile.level == 0 && tile.entry.is_some();
        let selected = top_level && self.sel == Some(tile.top);
        let body = self.shades.body(base, tile.level);
        p.rect_filled(r, RADIUS, body);
        let mut free = r.min.y;
        if tile.room.label {
            let head = Rect::from_min_size(r.min, vec2(r.width(), self.line + 2.0 * PAD));
            let fill = if selected {
                self.colors.fg(Token::BorderFocus)
            } else {
                self.shades.header(base)
            };
            let corners = CornerRadius {
                nw: RADIUS,
                ne: RADIUS,
                sw: 0,
                se: 0,
            };
            p.rect_filled(head.intersect(r), corners, fill);
            let marked = tile
                .entry
                .is_some_and(|(node, _)| self.s.marks.contains(node));
            let name_room = if marked { room - self.line } else { room };
            if marked {
                let c = pos2(head.max.x - PAD - self.line / 2.0, head.center().y);
                p.circle_filled(c, self.line * 0.25, self.colors.fg(Token::Mark));
            }
            let name = super::name_of(tile);
            let sizes = self.sizes(tile);
            let one_line = tile
                .nested
                .then(|| format!("{name}  {}", sizes[0]))
                .and_then(|t| self.line_of(ui, t, ink(fill), name_room, false));
            if let Some(g) = one_line.or_else(|| self.line_of(ui, name, ink(fill), name_room, true))
            {
                p.galley(at, g, ink(fill));
            }
            free = head.max.y;
            if !tile.nested && r.max.y - free >= self.line + PAD {
                let size =
                    (sizes.into_iter()).find_map(|t| self.line_of(ui, t, ink(body), room, false));
                if let Some(g) = size {
                    p.galley(pos2(at.x, free + 1.0), g, ink(body));
                    free += self.line + 1.0;
                }
                if r.max.y - free >= self.line + PAD
                    && let Some(text) = third_line(self.s, tile, self.today)
                    && let Some(g) =
                        self.line_of(ui, text, ink(body).gamma_multiply(0.7), room, false)
                {
                    p.galley(pos2(at.x, free + 1.0), g, ink(body));
                    free += self.line + 1.0;
                }
            }
        }
        if tile.candidate() {
            let below = Rect::from_min_max(pos2(r.min.x, free), r.max);
            hatch(&p, below, Stroke::new(1.0, base.gamma_multiply(0.6)));
        }
    }

    /// The colour tile `i` takes, from its token.
    fn colors_of(&self, tiles: &[Tile], i: usize) -> Color32 {
        self.colors.fg(self.base(tiles, i).0)
    }

    /// A tile's size with its share of what it is in, and the size alone for when that does
    /// not fit: a cut size would read as another number.
    fn sizes(&self, tile: &Tile) -> [String; 2] {
        let size = tile.live_size(self.s);
        let human = human(size);
        let share = (size.saturating_mul(100))
            .checked_div(tile.whole(self.s))
            .map_or(String::new(), |p| format!("  {p}%"));
        [format!("{human}{share}"), human]
    }

    /// The edge of the selected top-level tile of `tree`.
    pub fn outline_selected(&self, ui: &Ui, tree: &TileTree) {
        let Some(top) = self.sel else { return };
        let r = rect_of(tree.tiles[tree.tops[top]].rect).shrink(SEAM);
        let stroke = Stroke::new(2.0, self.colors.fg(Token::BorderFocus));
        (ui.painter()).rect_stroke(r, RADIUS, stroke, StrokeKind::Inside);
    }

    /// The thin edge of the top-level tile under the pointer.
    pub fn outline_hovered(&self, ui: &Ui, tile: &Tile) {
        let r = rect_of(tile.rect).shrink(SEAM);
        let stroke = Stroke::new(1.0, self.colors.fg(Token::Fg));
        (ui.painter()).rect_stroke(r, RADIUS, stroke, StrokeKind::Inside);
    }

    /// A square of the legend in `color`, hatched for cleanup candidates.
    pub fn swatch(&self, ui: &mut Ui, color: Color32, hatched: bool) {
        let side = self.line * 0.8;
        let (r, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
        let p = ui.painter().with_clip_rect(r);
        if hatched {
            p.rect_filled(r, RADIUS, self.shades.body(color, 0));
            hatch(&p, r, Stroke::new(1.0, color));
        } else {
            p.rect_filled(r, RADIUS, color);
        }
    }
}

/// Diagonal lines across `r` in `stroke`, clipped to it.
fn hatch(p: &egui::Painter, r: Rect, stroke: Stroke) {
    if r.height() <= 0.0 || r.width() <= 0.0 {
        return;
    }
    let p = p.with_clip_rect(p.clip_rect().intersect(r));
    let h = r.height();
    let mut d = -h;
    while d < r.width() {
        let from = pos2(r.min.x + d, r.max.y);
        let to = pos2(r.min.x + d + h, r.min.y);
        p.line_segment([from, to], stroke);
        d += HATCH;
    }
}

/// The third line of a leaf tile: its age band, or its own match's category and safety.
pub fn third_line(s: &SessionState, tile: &Tile, today: i64) -> Option<String> {
    if tile.small > 0 {
        return None;
    }
    match map::color_shown(s) {
        ColorBy::Age => Some(format!("age: {}", AGES[map::age_band(tile.mtime, today)].1)),
        ColorBy::Category => {
            let (rules, tag) = (s.snap.rules.as_ref()?, tile.tag?);
            (!tag.inherited).then(|| {
                let safety = tag.class.safety.as_str();
                format!("{} · {safety}", tag.category(rules).0)
            })
        }
        ColorBy::Folder => None,
    }
}

/// The name of a category in the legend, as its theme token is named.
pub fn category_name(c: Category) -> &'static str {
    match c {
        Category::Cache => "cache",
        Category::Build => "build",
        Category::Vm => "vm",
        Category::Toolchain => "toolchain",
        Category::Media => "media",
        Category::Download => "download",
        Category::Container => "container",
        Category::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_on_their_fill_and_tints_mix_towards_the_ground() {
        assert_eq!(ink(Color32::from_rgb(0x10, 0x12, 0x16)), Color32::WHITE);
        assert_eq!(ink(Color32::from_rgb(0xf0, 0xe0, 0xa0)), Color32::BLACK);
        let (base, ground) = (Color32::from_rgb(200, 100, 0), Color32::from_rgb(0, 0, 0));
        assert_eq!(mix(base, ground, 0.5), Color32::from_rgb(100, 50, 0));
        let dark = Shades { dark: true, ground };
        assert_ne!(dark.body(base, 0), dark.body(base, 3), "depth shows");
        assert_eq!(category_name(Category::Build), "build");
    }
}
