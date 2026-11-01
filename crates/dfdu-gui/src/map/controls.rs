//! The row above the tiles: the colouring, chosen here or with `p`; the depth, a button each
//! way; and on the right what the colours mean (the categories on screen, or the age bands)
//! and the mark of cleanup candidates.

use dfdu_session::SessionState;
use dfdu_session::actions::Action;
use dfdu_session::palette::{Category, Token};
use dfdu_session::treemap::{self as map, AGES, ColorBy, Tile};
use egui::{
    Button, Color32, CornerRadius, FontFamily, FontId, Response, Sense, Stroke, Ui, Vec2,
    WidgetInfo, vec2,
};

use super::paint::{self, Painter};
use crate::browser::Intent;
use crate::kit;
use crate::list::category;
use crate::theme::Colors;
use crate::theme::style::{
    MONO_SEMIBOLD_FAMILY, RADIUS_MD, SANS_MEDIUM_FAMILY, SPACE_2, SPACE_3, SPACE_4,
};

/// The row's height.
pub const HEIGHT: f32 = 44.0;
/// The side of a depth button.
const STEP: f32 = 24.0;
/// What the hatching means.
const CANDIDATE: &str = "cleanup candidate";

/// A colouring's name on its segment.
fn mode_name(c: ColorBy) -> &'static str {
    match c {
        ColorBy::Category => "Category",
        ColorBy::Folder => "Folder",
        ColorBy::Age => "Age",
    }
}

/// The colourings offered, in the order `p` steps through them: by category only with the
/// rules.
fn modes(s: &SessionState) -> Vec<ColorBy> {
    [ColorBy::Category, ColorBy::Age, ColorBy::Folder]
        .into_iter()
        .filter(|&c| c != ColorBy::Category || s.snap.rules.is_some())
        .collect()
}

/// Draws the row in `ui`, a left-to-right layout as wide as the tiles.
pub fn show(
    ui: &mut Ui,
    s: &SessionState,
    colors: &Colors,
    painter: &Painter,
    layers: &[(&[Tile], Option<usize>)],
    out: &mut Vec<Intent>,
) {
    ui.spacing_mut().item_spacing.x = SPACE_2;
    let dim = colors.fg(Token::FgDim);
    ui.label(kit::sans("Colour by", 12.0).color(dim));
    let modes = modes(s);
    let shown = map::color_shown(s);
    let names: Vec<&str> = modes.iter().map(|&c| mode_name(c)).collect();
    let at = modes.iter().position(|&c| c == shown).unwrap_or(0);
    if let Some(i) = kit::segmented(ui, colors, "Colour by", &names, at) {
        out.push(Intent::MapColor(modes[i]));
    }
    kit::key_cap(ui, colors, "p");
    ui.add_space(SPACE_4 - SPACE_2);
    ui.label(kit::sans("Depth", 12.0).color(dim));
    let less = s.map.depth > map::MIN_DEPTH;
    if step(ui, colors, "\u{2212}", less, "One level less").clicked() {
        out.push(Intent::Act(Action::TreemapShallower));
    }
    depth(ui, colors, s.map.depth);
    let more = s.map.depth < map::MAX_DEPTH;
    if step(ui, colors, "+", more, "One level deeper").clicked() {
        out.push(Intent::Act(Action::TreemapDeeper));
    }
    legend(ui, colors, &key_of(s, colors, painter, layers));
}

/// A depth button: `text` in a square, outlined on a raised fill.
fn step(ui: &mut Ui, colors: &Colors, text: &str, enabled: bool, hint: &str) -> Response {
    let b = Button::new(kit::mono_bold(text, 12.0).color(colors.fg(Token::Fg)))
        .fill(colors.chrome.raised)
        .stroke(Stroke::new(1.0, colors.fg(Token::Border)))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .min_size(vec2(STEP, STEP));
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = Vec2::ZERO;
        ui.add_enabled(enabled, b)
    })
    .inner
    .on_hover_text(hint)
}

/// The depth, in mono, named `Depth n`.
fn depth(ui: &mut Ui, colors: &Colors, depth: u8) {
    let fg = colors.fg(Token::Fg);
    let font = FontId::new(12.0, FontFamily::Name(MONO_SEMIBOLD_FAMILY.into()));
    let g = ui.painter().layout_no_wrap(depth.to_string(), font, fg);
    let (rect, r) = ui.allocate_exact_size(vec2(g.size().x.max(8.0), STEP), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, format!("Depth {depth}")));
    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
}

/// What the colours on screen mean: the categories of the tiles in `layers` in the order they
/// first appear, or the age bands; nothing for top folders.
fn key_of(
    s: &SessionState,
    colors: &Colors,
    painter: &Painter,
    layers: &[(&[Tile], Option<usize>)],
) -> Vec<(&'static str, Color32)> {
    match map::color_shown(s) {
        ColorBy::Category => {
            let mut slots = Vec::new();
            for &(tiles, _) in layers {
                for i in 0..tiles.len() {
                    if let Some(slot) = painter.base(tiles, i).1
                        && !slots.contains(&slot)
                    {
                        slots.push(slot);
                    }
                }
            }
            (slots.into_iter())
                .map(|slot| {
                    let c = Category::ALL.get(slot).copied().unwrap_or(Category::Other);
                    (paint::category_name(c), colors.fg(category(slot)))
                })
                .collect()
        }
        ColorBy::Age => (AGES.iter().enumerate())
            .map(|(band, (_, name))| (*name, colors.fg(map::age_token(band))))
            .collect(),
        ColorBy::Folder => Vec::new(),
    }
}

/// The legend, flush right: a chip per colour in `key` that fits, then the hatched square.
fn legend(ui: &mut Ui, colors: &Colors, key: &[(&str, Color32)]) {
    let font = FontId::new(12.0, FontFamily::Name(SANS_MEDIUM_FAMILY.into()));
    let width = |text: &str| {
        let g = (ui.painter()).layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE);
        g.size().x
    };
    // A chip is an 8px square and its name 6px apart; the hatched square is 10px.
    let chips: Vec<f32> = key.iter().map(|(name, _)| 14.0 + width(name)).collect();
    let mut total = 16.0 + width(CANDIDATE);
    let room = ui.available_width() - SPACE_4;
    if total > room {
        return;
    }
    let shown = fitting(&chips, total, SPACE_3, room);
    total += chips[..shown].iter().map(|w| w + SPACE_3).sum::<f32>();
    ui.add_space((ui.available_width() - total - 1.0).max(0.0));
    ui.spacing_mut().item_spacing.x = SPACE_3;
    for &(name, color) in &key[..shown] {
        kit::chip(ui, name, color);
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        paint::candidate_swatch(ui, colors);
        ui.label(kit::label(CANDIDATE, 12.0).color(colors.fg(Token::FgDim)));
    });
}

/// How many of the items `widths` wide fit, in order, `gap` apart, beside one `taken` wide
/// within `room`.
fn fitting(widths: &[f32], taken: f32, gap: f32, room: f32) -> usize {
    let mut total = taken;
    widths
        .iter()
        .take_while(|&&w| {
            total += gap + w;
            total <= room
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legend_keeps_the_chips_that_fit_in_order() {
        assert_eq!(fitting(&[10.0, 10.0, 10.0], 20.0, 5.0, 50.0), 2);
        assert_eq!(fitting(&[10.0, 40.0, 10.0], 20.0, 5.0, 100.0), 3);
        assert_eq!(fitting(&[90.0, 1.0], 20.0, 5.0, 100.0), 0);
        assert_eq!(mode_name(ColorBy::Folder), "Folder");
    }
}
