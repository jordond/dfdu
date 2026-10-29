//! The colours of the tiles. With 256 colours or more tiles are filled ([`Paint::Fill`]): each
//! tile's base colour (its category, age band or top folder) tints the background, more
//! strongly on a header row and the deeper the tile is, so depth reads as shading
//! ([`Shades`]). On a dark terminal the tints are dark and the labels light; on a light one the
//! other way round; a label takes whichever of black and white contrasts more with its fill,
//! which is always at least WCAG's 4.5:1. With fewer colours (`ansi`, `mono`) there are no
//! shades to give, and tiles keep light frames and are filled with shade glyphs by depth
//! ([`Paint::Glyphs`]).

use std::collections::HashMap;

use ratatui::style::{Color, Style};

use crate::theme::{Bars, ColorLevel, Rgb, Theme, contrast, lightness, rgb_color};

/// How tiles are told apart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Paint {
    /// Background fills, at this colour level.
    Fill(ColorLevel),
    /// Frames and shade glyphs.
    Glyphs,
}

impl Paint {
    pub fn of(t: &Theme) -> Paint {
        match t.level() {
            level @ (ColorLevel::TrueColor | ColorLevel::Ansi256) => Paint::Fill(level),
            ColorLevel::Ansi16 | ColorLevel::None => Paint::Glyphs,
        }
    }
}

/// The glyphs that fill a tile without colour, by its depth: `█▓▒░`, or ASCII.
pub fn shade(bars: Bars, level: u8) -> &'static str {
    let set: [&str; 4] = match bars {
        Bars::Unicode => ["█", "▓", "▒", "░"],
        Bars::Ascii => ["#", "%", "+", "."],
    };
    set[usize::from(level).min(set.len() - 1)]
}

const BLACK: Rgb = (0, 0, 0);
const WHITE: Rgb = (0xff, 0xff, 0xff);
/// The ground tints mix into when the terminal's background is not known.
const DARK_GROUND: Rgb = (0x10, 0x12, 0x16);
/// How much of the ground a body takes, by depth: deeper tiles show more of their hue.
const DARK_BODY: [f64; 4] = [0.74, 0.68, 0.62, 0.56];
const LIGHT_BODY: [f64; 4] = [0.6, 0.5, 0.4, 0.3];
/// How much of the ground a header row takes on a dark terminal, and how far it goes towards
/// black on a light one: the hue at its strongest.
const DARK_HEADER: f64 = 0.56;
const LIGHT_HEADER: f64 = 0.15;

/// `a` moved the fraction `t` of the way to `b`, channel by channel.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a mix of two channels stays within 0..=255"
)]
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let ch = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// The tints of a theme: dark ones on a dark terminal, light ones on a light one.
#[derive(Copy, Clone, Debug)]
pub struct Shades {
    dark: bool,
    ground: Rgb,
}

impl Shades {
    /// The terminal's background when it was queried, else the theme's kind (`light` is light).
    pub fn of(t: &Theme) -> Shades {
        let bg = t.background();
        let dark = bg.map_or(!t.name().contains("light"), |b| lightness(b) < 0.5);
        let ground = bg.unwrap_or(if dark { DARK_GROUND } else { WHITE });
        Shades { dark, ground }
    }

    /// The fill of a tile's header row.
    pub fn header(self, base: Rgb) -> Rgb {
        if self.dark {
            mix(base, self.ground, DARK_HEADER)
        } else {
            mix(base, BLACK, LIGHT_HEADER)
        }
    }

    /// The fill of a tile's body at depth `level`.
    pub fn body(self, base: Rgb, level: u8) -> Rgb {
        let by = if self.dark { DARK_BODY } else { LIGHT_BODY };
        mix(base, self.ground, by[usize::from(level).min(by.len() - 1)])
    }
}

/// The label colour on `fill`: black or white, whichever contrasts more.
pub fn ink(fill: Rgb) -> Rgb {
    if contrast(WHITE, fill) >= contrast(BLACK, fill) {
        WHITE
    } else {
        BLACK
    }
}

/// Turns fills into colours of the terminal's level, remembering each (the nearest of 256
/// colours takes a search).
pub struct Colors {
    level: ColorLevel,
    seen: HashMap<Rgb, Color>,
}

impl Colors {
    pub fn new(level: ColorLevel) -> Self {
        Colors {
            level,
            seen: HashMap::new(),
        }
    }

    pub fn get(&mut self, c: Rgb) -> Color {
        let level = self.level;
        *self.seen.entry(c).or_insert_with(|| rgb_color(c, level))
    }

    /// Text on `fill`: the fill as background and the label colour that reads on it.
    pub fn on(&mut self, fill: Rgb) -> Style {
        Style::new().bg(self.get(fill)).fg(self.get(ink(fill)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color_rgb;

    /// Every category colour of the colour themes, at every depth and on its header.
    fn fills(t: &Theme) -> Vec<Rgb> {
        let s = Shades::of(t);
        let mut out = Vec::new();
        for base in t
            .cat
            .iter()
            .filter_map(|style| style.fg.and_then(color_rgb))
        {
            out.push(s.header(base));
            out.extend((0..4).map(|level| s.body(base, level)));
        }
        out
    }

    #[test]
    fn labels_read_on_every_fill() {
        for name in ["dark", "light", "high-contrast"] {
            let t = Theme::named(name, ColorLevel::TrueColor, None).expect(name);
            for fill in fills(&t) {
                let c = contrast(ink(fill), fill);
                assert!(c >= 4.5, "{name}: {fill:?} reads at {c:.2}");
            }
        }
    }

    #[test]
    fn dark_themes_get_dark_tints_and_light_ones_light_tints() {
        let dark = Theme::named("dark", ColorLevel::TrueColor, None).expect("dark");
        assert!(
            fills(&dark).iter().all(|&f| lightness(f) < 0.55),
            "dark tints"
        );
        assert!(
            fills(&dark).iter().all(|&f| ink(f) == WHITE),
            "light labels"
        );
        let light = Theme::named("light", ColorLevel::TrueColor, None).expect("light");
        let s = Shades::of(&light);
        let base = light.cat[0].fg.and_then(color_rgb).expect("a colour");
        assert!(lightness(s.body(base, 0)) > 0.7, "light tints");
        // A queried background decides over the name.
        let on_white = Theme::named("dark", ColorLevel::TrueColor, Some(WHITE)).expect("dark");
        assert!(!Shades::of(&on_white).dark);
    }

    #[test]
    fn deeper_bodies_show_more_of_their_hue() {
        let s = Shades {
            dark: true,
            ground: DARK_GROUND,
        };
        let base = (0x7a, 0xa2, 0xf7);
        assert!(lightness(s.body(base, 1)) > lightness(s.body(base, 0)));
        assert_eq!(
            s.body(base, 9),
            s.body(base, 3),
            "three is as deep as it goes"
        );
    }
}
