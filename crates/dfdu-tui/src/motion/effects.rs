//! The effects on finished frames, with tachyonfx: the flash of moved rows and the fade of
//! deleted ones. tachyonfx mixes colours in RGB and writes RGB colours into the cells, so the
//! colour effects run only at TrueColor; at other levels a flash reverses the row.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tachyonfx::{EffectTimer, Interpolation, ToRgbComponents, fx};

use super::{FADE, FLASH};
use crate::theme::{ColorLevel, Rgb, Theme};

/// Flashes `area`, `elapsed` after the flash started: at TrueColor its background fades from
/// a tint of the theme's accent back to its own colour, otherwise it is reversed throughout.
pub(super) fn flash(theme: &Theme, buf: &mut Buffer, area: Rect, elapsed: Duration) {
    if theme.level() != ColorLevel::TrueColor {
        for pos in area.positions() {
            if let Some(cell) = buf.cell_mut(pos) {
                cell.modifier.toggle(Modifier::REVERSED);
            }
        }
        return;
    }
    let base = background(theme);
    let accent = theme.bar.fg.and_then(rgb).unwrap_or(base);
    let from = mix(base, accent, 0.35);
    let timer = EffectTimer::new(FLASH, Interpolation::QuadOut);
    let mut effect = fx::effect_fn((from, base), timer, |&mut (from, base), ctx, cells| {
        let alpha = ctx.alpha();
        for (_, cell) in cells {
            let to = rgb(cell.bg).unwrap_or(base);
            cell.set_bg(color(mix(from, to, alpha)));
        }
    });
    effect.process(elapsed, buf, area);
}

/// Fades the text of `area` halfway from the dim colour to the background, `elapsed` after the
/// entry was deleted; the row then stays faded.
pub(super) fn fade(theme: &Theme, buf: &mut Buffer, area: Rect, elapsed: Duration) {
    let base = background(theme);
    let dim = theme.dim.fg.or(theme.fg.fg).and_then(rgb).unwrap_or(base);
    let timer = EffectTimer::new(FADE, Interpolation::QuadOut);
    fx::fade_to_fg(color(mix(dim, base, 0.5)), timer).process(elapsed, buf, area);
}

/// The terminal's background: as queried, or else black under a light text colour and white
/// under a dark one.
fn background(theme: &Theme) -> Rgb {
    theme.background().unwrap_or_else(|| {
        let (r, g, b) = theme.fg.fg.and_then(rgb).unwrap_or((255, 255, 255));
        if r as u16 + g as u16 + b as u16 > 3 * 127 {
            (0, 0, 0)
        } else {
            (255, 255, 255)
        }
    })
}

fn rgb(c: Color) -> Option<Rgb> {
    (c != Color::Reset).then(|| c.to_rgb())
}

fn color((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// `a` moved the fraction `t` of the way to `b`.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}
