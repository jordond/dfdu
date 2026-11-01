//! Painting the tiles, after the Terminal design: each a rounded rectangle with a gutter round
//! it. A leaf is filled with the colour its colouring gives it (its category, age band or top
//! folder, `dfdu_session::treemap::base`) with its name and size on it in `on.tile`; a tile
//! with its children inside is raised, with a header row for its name and size above them; the
//! tile of the entries merged for being small is an empty bar's colour with their count.
//! Cleanup candidates are hatched with dark diagonal lines. The selected tile has a focus
//! outline just outside it; the one under the pointer a thin edge.

use std::sync::Arc;

use dfdu_session::SessionState;
use dfdu_session::format::{count, human};
use dfdu_session::palette::{Category, Token};
use dfdu_session::treemap::{self as map, Tile, TileTree};
use egui::text::{Galley, LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

use super::rect_of;
use crate::theme::Colors;
use crate::theme::style::{RADIUS_SM, SANS_SEMIBOLD_FAMILY};

/// Half the gutter between neighbouring tiles: each tile gives up this much on every side.
pub const HALF_GUTTER: f32 = 2.0;
/// The header row of a tile with its children inside.
pub const HEADER: f32 = 18.0;
/// How far a tile's children sit inside it, below its header and from its other edges.
pub const INSET: f32 = 3.0;
/// Room between a tile's edge and its text.
const PAD: f32 = 5.0;
/// A leaf with room for its name, and with room for its size below it, across and down.
const NAME_MIN: Vec2 = vec2(36.0, 18.0);
const SIZE_MIN: Vec2 = vec2(64.0, 34.0);
/// Text narrower than this is left out rather than cut to a stub.
const TEXT_MIN: f32 = 18.0;
/// Hatching: lines this thick, this far apart across them, in black at a fifth.
const HATCH_WIDTH: f32 = 2.0;
const HATCH_GAP: f32 = 6.0;
const HATCH_INK: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 51);
/// The side of the legend's hatched square.
const SWATCH: f32 = 10.0;

/// The rectangle tile `tile` is painted in: its place less the gutter.
pub fn painted(tile: &Tile) -> Rect {
    rect_of(tile.rect).shrink(HALF_GUTTER)
}

/// What a leaf painted at `size` has room for: its name, and its size below the name.
pub fn labels(size: Vec2) -> (bool, bool) {
    let name = size.x >= NAME_MIN.x && size.y >= NAME_MIN.y;
    (name, name && size.x >= SIZE_MIN.x && size.y >= SIZE_MIN.y)
}

/// What paints the tiles of one frame.
pub struct Painter<'a> {
    s: &'a SessionState,
    colors: &'a Colors,
    /// The selected top-level tile.
    sel: Option<usize>,
    name: FontId,
    mono: FontId,
    today: i64,
}

impl<'a> Painter<'a> {
    pub fn new(s: &'a SessionState, colors: &'a Colors, sel: Option<usize>) -> Self {
        Painter {
            s,
            colors,
            sel,
            name: FontId::new(11.0, FontFamily::Name(SANS_SEMIBOLD_FAMILY.into())),
            mono: FontId::new(11.0, FontFamily::Monospace),
            today: dfdu_tree::now_secs(),
        }
    }

    /// The token tile `i` of `tiles` takes its colour from, and its category colour slot.
    pub fn base(&self, tiles: &[Tile], i: usize) -> (Token, Option<usize>) {
        map::base(self.s, tiles, i, self.today)
    }

    /// `text` in `font` and `color` as one line no wider than `room`, cut with an ellipsis when
    /// `cut`; `None` when it does not fit, or there is too little room for more than a stub.
    fn line(
        &self,
        ui: &Ui,
        text: String,
        font: &FontId,
        color: Color32,
        room: f32,
        cut: bool,
    ) -> Option<Arc<Galley>> {
        if room < TEXT_MIN {
            return None;
        }
        let mut job = LayoutJob::single_section(text, TextFormat::simple(font.clone(), color));
        if cut {
            job.wrap = TextWrapping::truncate_at_width(room);
        }
        let g = ui.painter().layout_job(job);
        (cut || g.size().x <= room).then_some(g)
    }

    /// Paints tile `i` of `tiles`.
    pub fn tile(&self, ui: &Ui, tiles: &[Tile], i: usize) {
        let tile = &tiles[i];
        let r = painted(tile);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
        if tile.small > 0 {
            self.fold(ui, &p, tile, r);
        } else if tile.nested {
            self.parent(ui, &p, tile, r);
        } else {
            self.leaf(ui, &p, tiles, i, r);
        }
    }

    /// A leaf: filled in its colour, hatched when a cleanup candidate, its name and size on it.
    fn leaf(&self, ui: &Ui, p: &egui::Painter, tiles: &[Tile], i: usize, r: Rect) {
        let tile = &tiles[i];
        let fill = self.colors.fg(self.base(tiles, i).0);
        p.rect_filled(r, CornerRadius::same(RADIUS_SM), fill);
        if tile.candidate() {
            hatch(p, r);
        }
        let marked = self.marked(tile);
        if marked {
            self.mark(p, pos2(r.max.x - PAD - 3.0, r.min.y + PAD + 2.0));
        }
        let (name, size) = labels(r.size());
        let ink = self.colors.chrome.on_tile;
        let room = r.width() - 2.0 * PAD - if marked { 10.0 } else { 0.0 };
        let mut y = r.min.y + 3.0;
        if name && let Some(g) = self.line(ui, super::name_of(tile), &self.name, ink, room, true) {
            y += g.size().y;
            p.galley(pos2(r.min.x + PAD, r.min.y + 3.0), g, ink);
        }
        if size {
            let text = human(tile.live_size(self.s));
            let room = r.width() - 2.0 * PAD;
            if let Some(g) = self.line(ui, text, &self.mono, ink, room, false) {
                p.galley(pos2(r.min.x + PAD, y + 1.0), g, ink);
            }
        }
    }

    /// A tile with its children inside: raised, its name and size in a header row above them.
    fn parent(&self, ui: &Ui, p: &egui::Painter, tile: &Tile, r: Rect) {
        let c = &self.colors.chrome;
        p.rect_filled(r, CornerRadius::same(RADIUS_SM), c.raised);
        let head = Rect::from_min_size(r.min, vec2(r.width(), HEADER));
        let mut right = head.max.x - PAD;
        if self.marked(tile) {
            self.mark(p, pos2(right - 3.0, head.center().y));
            right -= 10.0;
        }
        let dim = self.colors.fg(Token::FgDim);
        let size = human(tile.live_size(self.s));
        let size = self.line(ui, size, &self.mono, dim, right - head.min.x - PAD, false);
        let name_room = |taken: f32| right - taken - head.min.x - PAD;
        // The size goes on the right only when the name keeps room beside it.
        let size = size.filter(|g| name_room(g.size().x + 8.0) >= 3.0 * TEXT_MIN);
        if let Some(g) = &size {
            let at = pos2(right - g.size().x, head.center().y - g.size().y / 2.0);
            p.galley(at, g.clone(), dim);
            right -= g.size().x + 8.0;
        }
        let header = self.colors.fg(Token::Header);
        let room = right - head.min.x - PAD;
        if let Some(g) = self.line(ui, super::name_of(tile), &self.name, header, room, true) {
            let at = pos2(head.min.x + PAD, head.center().y - g.size().y / 2.0);
            p.galley(at, g, header);
        }
    }

    /// The tile of the entries merged for being small: how many, and their size below.
    fn fold(&self, ui: &Ui, p: &egui::Painter, tile: &Tile, r: Rect) {
        p.rect_filled(
            r,
            CornerRadius::same(RADIUS_SM),
            self.colors.fg(Token::BarEmpty),
        );
        let fg = self.colors.fg(Token::Fg);
        let room = r.width() - 2.0 * PAD;
        let (name, size) = labels(r.size());
        let more = format!("+{}", count(tile.small));
        let Some(g) = self
            .line(ui, more, &self.mono, fg, room, false)
            .filter(|_| name)
        else {
            return;
        };
        let y = r.min.y + 3.0 + g.size().y;
        p.galley(pos2(r.min.x + PAD, r.min.y + 3.0), g, fg);
        if size && let Some(g) = self.line(ui, human(tile.size), &self.mono, fg, room, false) {
            p.galley(pos2(r.min.x + PAD, y + 1.0), g, fg);
        }
    }

    /// The tile is a top-level one whose row is marked.
    fn marked(&self, tile: &Tile) -> bool {
        tile.level == 0
            && tile
                .entry
                .is_some_and(|(node, _)| self.s.marks.contains(node))
    }

    /// The dot of a marked tile, centred on `at`.
    fn mark(&self, p: &egui::Painter, at: egui::Pos2) {
        p.circle(
            at,
            3.0,
            self.colors.fg(Token::Mark),
            Stroke::new(1.0, self.colors.chrome.on_tile),
        );
    }

    /// The focus outline of the selected top-level tile of `tree`, just outside it.
    pub fn outline_selected(&self, ui: &Ui, tree: &TileTree) {
        let Some(top) = self.sel else { return };
        let r = painted(&tree.tiles[tree.tops[top]]).expand(1.0);
        let stroke = Stroke::new(2.0, self.colors.fg(Token::BorderFocus));
        let radius = CornerRadius::same(RADIUS_SM + 1);
        (ui.painter()).rect_stroke(r, radius, stroke, StrokeKind::Outside);
    }

    /// The thin edge of the top-level tile under the pointer.
    pub fn outline_hovered(&self, ui: &Ui, tile: &Tile) {
        let stroke = Stroke::new(1.0, self.colors.fg(Token::Fg).gamma_multiply(0.7));
        let radius = CornerRadius::same(RADIUS_SM);
        (ui.painter()).rect_stroke(painted(tile), radius, stroke, StrokeKind::Inside);
    }
}

/// The legend's square for cleanup candidates: hatched.
pub fn candidate_swatch(ui: &mut Ui, colors: &Colors) {
    let (r, _) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
    let p = ui.painter().with_clip_rect(r);
    p.rect_filled(r, CornerRadius::same(2), colors.fg(Token::FgDim));
    hatch(&p, r);
}

/// The horizontal step between hatching lines `gap` apart across them, at 45 degrees.
fn hatch_step(gap: f32) -> f32 {
    gap * std::f32::consts::SQRT_2
}

/// Dark diagonal lines across `r`, rising to the right, clipped to it.
fn hatch(p: &egui::Painter, r: Rect) {
    if r.height() <= 0.0 || r.width() <= 0.0 {
        return;
    }
    let p = p.with_clip_rect(p.clip_rect().intersect(r));
    let stroke = Stroke::new(HATCH_WIDTH, HATCH_INK);
    let h = r.height();
    let mut d = -h;
    while d < r.width() {
        p.line_segment(
            [pos2(r.min.x + d, r.max.y), pos2(r.min.x + d + h, r.min.y)],
            stroke,
        );
        d += hatch_step(HATCH_GAP);
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
    fn a_leaf_names_itself_then_gives_its_size_as_room_allows() {
        assert_eq!(labels(vec2(30.0, 40.0)), (false, false), "too narrow");
        assert_eq!(labels(vec2(36.0, 18.0)), (true, false), "a name");
        assert_eq!(labels(vec2(63.0, 60.0)), (true, false), "no room across");
        assert_eq!(labels(vec2(64.0, 34.0)), (true, true), "a name and a size");
        assert_eq!(category_name(Category::Build), "build");
    }

    #[test]
    fn hatching_lines_are_their_gap_apart_across_them() {
        let across = hatch_step(HATCH_GAP) / std::f32::consts::SQRT_2;
        assert!((across - HATCH_GAP).abs() < 1e-4);
    }
}
