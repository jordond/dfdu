//! The effects on finished frames, with tachyonfx: the flash of moved rows and the fade of
//! deleted ones. The flash mixes its colours in RGB here; tachyonfx's fade blends in HSL, its
//! default colour space. Both write RGB colours into the cells, so the colour effects run only
//! where [`rgb_effects`] says; elsewhere a flash reverses the row.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tachyonfx::{EffectTimer, Interpolation, ToRgbComponents, fx};

use super::{FADE, FLASH};
use crate::theme::{ColorLevel, Rgb, Theme};

/// True when the colour effects run: at `TrueColor`, on a background that is known (queried) or
/// that the theme's RGB text colour was chosen for. A theme that leaves the text colour to the
/// terminal (`ansi`, `high-contrast`) on an unknown background would be mixed towards a guess.
pub(super) fn rgb_effects(theme: &Theme) -> bool {
    theme.level() == ColorLevel::TrueColor
        && (theme.background().is_some() || matches!(theme.fg.fg, Some(Color::Rgb(..))))
}

/// Flashes `area`, `elapsed` after the flash started: with [`rgb_effects`] its background fades
/// from a tint of the theme's accent back to its own colour, otherwise it is reversed
/// throughout.
pub(super) fn flash(theme: &Theme, buf: &mut Buffer, area: Rect, elapsed: Duration) {
    if !rgb_effects(theme) {
        for pos in area.positions() {
            if let Some(cell) = buf.cell_mut(pos) {
                cell.modifier.toggle(Modifier::REVERSED);
            }
        }
        return;
    }
    let base = background(theme);
    let accent = theme.bar_fill.fg.and_then(rgb).unwrap_or(base);
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
/// entry was deleted; the row then stays faded. Only with [`rgb_effects`], and only when the
/// theme has a dim or text colour: the terminal's own text colour is not known, and a fade
/// from the background to itself would hide the row.
pub(super) fn fade(theme: &Theme, buf: &mut Buffer, area: Rect, elapsed: Duration) {
    let Some(dim) = theme.fg_dim.fg.or(theme.fg.fg).and_then(rgb) else {
        return;
    };
    let base = background(theme);
    let timer = EffectTimer::new(FADE, Interpolation::QuadOut);
    fx::fade_to_fg(color(mix(dim, base, 0.5)), timer).process(elapsed, buf, area);
}

/// The terminal's background: as queried, or else black under a light text colour and white
/// under a dark one.
fn background(theme: &Theme) -> Rgb {
    theme.background().unwrap_or_else(|| {
        let (r, g, b) = theme.fg.fg.and_then(rgb).unwrap_or((255, 255, 255));
        if u16::from(r) + u16::from(g) + u16::from(b) > 3 * 127 {
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
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "t is in 0..=1, so each channel stays between the two u8 channels it mixes"
)]
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str, background: Option<Rgb>) -> Theme {
        Theme::named(name, ColorLevel::TrueColor, background).expect(name)
    }

    #[test]
    fn mix_moves_each_channel_the_fraction_of_the_way() {
        let (black, white) = ((0, 0, 0), (255, 255, 255));
        assert_eq!(mix(black, white, 0.0), black);
        assert_eq!(mix(black, white, 1.0), white);
        assert_eq!(mix(black, white, 0.5), (128, 128, 128));
        assert_eq!(mix((10, 200, 30), (30, 100, 30), 0.5), (20, 150, 30));
    }

    #[test]
    fn background_is_the_queried_one_or_a_guess_from_the_text_colour() {
        assert_eq!(background(&theme("dark", Some((1, 2, 3)))), (1, 2, 3));
        assert_eq!(background(&theme("dark", None)), (0, 0, 0));
        assert_eq!(background(&theme("light", None)), (255, 255, 255));
        assert_eq!(rgb(Color::Reset), None);
        assert_eq!(rgb(color((4, 5, 6))), Some((4, 5, 6)));
    }

    #[test]
    fn without_colour_effects_a_flash_reverses_the_row() {
        let ansi = Theme::ansi16();
        assert!(!rgb_effects(&ansi));
        assert!(rgb_effects(&theme("dark", None)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        flash(&ansi, &mut buf, Rect::new(0, 1, 4, 1), Duration::ZERO);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(3, 1)].modifier.contains(Modifier::REVERSED));
    }
}
