//! The colours of the tiles. With 256 colours or more tiles are filled ([`Paint::Fill`]): each
//! tile's base colour (its category, age band or top folder) as the background, in a darker
//! shade on a header row and lighter the deeper the tile is, so depth reads as shading; labels
//! are black or white by the fill's lightness. With fewer colours (`ansi`, `mono`) there are no
//! shades to give, and tiles keep light frames and are filled with shade glyphs by depth
//! ([`Paint::Glyphs`]).

use std::collections::HashMap;

use ratatui::style::{Color, Style};

use crate::theme::{Bars, ColorLevel, Rgb, Theme, lightness, rgb_color};

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
/// How far a header row goes towards black.
const HEADER_DARKER: f64 = 0.35;
/// How far a body goes towards white, by depth.
const BODY_LIGHTER: [f64; 4] = [0.0, 0.2, 0.36, 0.5];
/// Fills lighter than this get black labels, the others white.
const LIGHT_FILL: f64 = 0.6;

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

/// The fill of a tile's header row.
pub fn header(base: Rgb) -> Rgb {
    mix(base, BLACK, HEADER_DARKER)
}

/// The fill of a tile's body at depth `level`.
pub fn body(base: Rgb, level: u8) -> Rgb {
    let t = BODY_LIGHTER[usize::from(level).min(BODY_LIGHTER.len() - 1)];
    mix(base, WHITE, t)
}

/// The label colour on `fill`: black on light fills, white on dark ones.
pub fn ink(fill: Rgb) -> Rgb {
    if lightness(fill) > LIGHT_FILL {
        (0x10, 0x10, 0x10)
    } else {
        (0xf5, 0xf5, 0xf5)
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

    #[test]
    fn headers_are_darker_and_deeper_bodies_lighter() {
        let base = (0x7a, 0xa2, 0xf7);
        assert!(lightness(header(base)) < lightness(body(base, 0)));
        assert!(lightness(body(base, 1)) > lightness(body(base, 0)));
        assert!(lightness(body(base, 3)) > lightness(body(base, 2)));
        assert_eq!(
            body(base, 9),
            body(base, 3),
            "deeper than three is as light as three"
        );
    }

    #[test]
    fn labels_contrast_with_their_fill() {
        assert_eq!(ink(WHITE), (0x10, 0x10, 0x10));
        assert_eq!(ink((0x20, 0x20, 0x40)), (0xf5, 0xf5, 0xf5));
    }
}
