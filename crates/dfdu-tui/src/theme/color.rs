//! Colour levels, colours with a value per level, and the conversions between levels.

use ratatui::style::Color;

/// An sRGB colour with 8 bits per channel.
pub type Rgb = (u8, u8, u8);

/// How many colours the terminal can show, from none to 24-bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColorLevel {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

/// A colour with optional explicit values for each level. Resolving it at a level uses that
/// level's value when given and otherwise converts from the richest value it has (a colour
/// with only poorer values uses those unchanged: a named ANSI colour stays named at every
/// level).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorSpec {
    pub rgb: Option<Rgb>,
    pub ansi256: Option<u8>,
    /// One of the 16 named colours or `Color::Reset`.
    pub ansi16: Option<Color>,
}

impl ColorSpec {
    /// No colour at any level: the cell keeps whatever colour it had.
    pub const NONE: ColorSpec = ColorSpec {
        rgb: None,
        ansi256: None,
        ansi16: None,
    };

    pub const fn named(c: Color) -> Self {
        ColorSpec {
            ansi16: Some(c),
            ..Self::NONE
        }
    }

    pub fn resolve(&self, level: ColorLevel) -> Option<Color> {
        let indexed = self.ansi256.map(Color::Indexed);
        match level {
            ColorLevel::None => None,
            ColorLevel::TrueColor => self
                .rgb
                .map(|(r, g, b)| Color::Rgb(r, g, b))
                .or(indexed)
                .or(self.ansi16),
            ColorLevel::Ansi256 => indexed
                .or_else(|| self.rgb.map(|c| Color::Indexed(rgb_to_256(c))))
                .or(self.ansi16),
            ColorLevel::Ansi16 => self
                .ansi16
                .or_else(|| self.rgb.map(rgb_to_16))
                .or_else(|| self.ansi256.map(ansi256_to_16)),
        }
    }
}

/// The 16 named colours with xterm's default values, in palette order.
const ANSI16: [(Color, Rgb); 16] = [
    (Color::Black, (0x00, 0x00, 0x00)),
    (Color::Red, (0xcd, 0x00, 0x00)),
    (Color::Green, (0x00, 0xcd, 0x00)),
    (Color::Yellow, (0xcd, 0xcd, 0x00)),
    (Color::Blue, (0x00, 0x00, 0xee)),
    (Color::Magenta, (0xcd, 0x00, 0xcd)),
    (Color::Cyan, (0x00, 0xcd, 0xcd)),
    (Color::Gray, (0xe5, 0xe5, 0xe5)),
    (Color::DarkGray, (0x7f, 0x7f, 0x7f)),
    (Color::LightRed, (0xff, 0x00, 0x00)),
    (Color::LightGreen, (0x00, 0xff, 0x00)),
    (Color::LightYellow, (0xff, 0xff, 0x00)),
    (Color::LightBlue, (0x5c, 0x5c, 0xff)),
    (Color::LightMagenta, (0xff, 0x00, 0xff)),
    (Color::LightCyan, (0x00, 0xff, 0xff)),
    (Color::White, (0xff, 0xff, 0xff)),
];

/// The hue families of the 16 colours, as (normal, bright) palette indexes.
const HUES: [(usize, usize); 6] = [(1, 9), (3, 11), (2, 10), (6, 14), (4, 12), (5, 13)];
/// The neutral colours: black, bright black, white and bright white.
const GREYS: [usize; 4] = [0, 8, 7, 15];
/// `OKLab` chroma below which a colour counts as grey.
const GREY_CHROMA: f64 = 0.03;

/// The colour xterm shows for a 256-colour index by default.
pub fn ansi256_rgb(i: u8) -> Rgb {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..16 => ANSI16[i as usize].1,
        16..232 => {
            let i = (i - 16) as usize;
            (CUBE[i / 36], CUBE[i / 6 % 6], CUBE[i % 6])
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

/// The nearest entry of the 6×6×6 cube and the grey ramp (indexes 16 to 255), by distance in
/// `OKLab`. Indexes 0 to 15 are left out because terminals give them their own colours.
pub fn rgb_to_256(c: Rgb) -> u8 {
    let target = oklab(c);
    (16..=255)
        .min_by(|&a, &b| {
            let da = distance(target, oklab(ansi256_rgb(a)));
            let db = distance(target, oklab(ansi256_rgb(b)));
            da.total_cmp(&db)
        })
        .unwrap_or(16)
}

/// The closest of the 16 named colours: greys by lightness, other colours by hue, taking the
/// bright variant only for colours at least as light as it (the normal variants read on more
/// backgrounds).
#[expect(
    clippy::many_single_char_names,
    reason = "l, a and b are the Oklab axes"
)]
pub fn rgb_to_16(c: Rgb) -> Color {
    let [l, a, b] = oklab(c);
    if a.hypot(b) < GREY_CHROMA {
        let i = GREYS
            .into_iter()
            .min_by(|&x, &y| {
                let dx = (oklab(ANSI16[x].1)[0] - l).abs();
                let dy = (oklab(ANSI16[y].1)[0] - l).abs();
                dx.total_cmp(&dy)
            })
            .unwrap_or(0);
        return ANSI16[i].0;
    }
    let hue = b.atan2(a);
    let (normal, bright) = HUES
        .into_iter()
        .min_by(|&(x, _), &(y, _)| hue_gap(hue, x).total_cmp(&hue_gap(hue, y)))
        .unwrap_or(HUES[0]);
    if l >= oklab(ANSI16[bright].1)[0] {
        ANSI16[bright].0
    } else {
        ANSI16[normal].0
    }
}

/// The named colour for indexes 0 to 15; the nearest named colour for the others.
pub fn ansi256_to_16(i: u8) -> Color {
    match i {
        0..16 => ANSI16[i as usize].0,
        _ => rgb_to_16(ansi256_rgb(i)),
    }
}

/// WCAG 2.1 relative luminance, 0 for black to 1 for white.
pub fn relative_luminance(c: Rgb) -> f64 {
    let (r, g, b) = linear(c);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG 2.1 contrast ratio, 1 to 21.
#[cfg(test)]
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Perceived lightness (CIE L*) scaled to 0..=1, where 0.5 is the perceptual middle grey.
pub fn lightness(c: Rgb) -> f64 {
    const E: f64 = 216.0 / 24389.0;
    const K: f64 = 24389.0 / 27.0;
    let y = relative_luminance(c);
    let l = if y > E {
        116.0 * y.cbrt() - 16.0
    } else {
        K * y
    };
    l / 100.0
}

fn linear((r, g, b): Rgb) -> (f64, f64, f64) {
    fn channel(v: u8) -> f64 {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    (channel(r), channel(g), channel(b))
}

#[expect(
    clippy::many_single_char_names,
    reason = "the names of the Oklab reference conversion: r, g, b in, l, m, s cone responses"
)]
fn oklab(c: Rgb) -> [f64; 3] {
    let (r, g, b) = linear(c);
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    ]
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Angle between `hue` (radians) and the hue of palette colour `i`.
fn hue_gap(hue: f64, i: usize) -> f64 {
    let [_, a, b] = oklab(ANSI16[i].1);
    let d = (hue - b.atan2(a)).abs();
    d.min(std::f64::consts::TAU - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_downsamples_to_nearest_256_and_16() {
        let cases = [
            ((0xe6, 0x9f, 0x00), 178, Color::Yellow),
            ((0x00, 0x72, 0xb2), 25, Color::Blue),
            ((0xcc, 0x79, 0xa7), 175, Color::Magenta),
            ((0x00, 0x9e, 0x73), 36, Color::Green),
            ((0xd5, 0x5e, 0x00), 166, Color::Red),
            ((0xff, 0x00, 0x00), 196, Color::LightRed),
            ((0x99, 0x99, 0x99), 247, Color::DarkGray),
        ];
        for (rgb, i256, i16) in cases {
            assert_eq!(rgb_to_256(rgb), i256, "{rgb:02x?} at 256");
            assert_eq!(rgb_to_16(rgb), i16, "{rgb:02x?} at 16");
        }
        for (v, i) in [
            (0x08, 232),
            (0x3a, 237),
            (0x80, 244),
            (0xb2, 249),
            (0xee, 255),
        ] {
            assert_eq!(rgb_to_256((v, v, v)), i, "grey {v:#04x}");
        }
        assert_eq!(rgb_to_256((0, 0, 0)), 16);
        assert_eq!(rgb_to_256((255, 255, 255)), 231);
        assert_eq!(rgb_to_16((0, 0, 0)), Color::Black);
        assert_eq!(rgb_to_16((0x20, 0x20, 0x20)), Color::Black);
        assert_eq!(rgb_to_16((0xd0, 0xd0, 0xd0)), Color::Gray);
        assert_eq!(rgb_to_16((255, 255, 255)), Color::White);
        assert_eq!(ansi256_to_16(178), Color::Yellow);
        assert_eq!(ansi256_to_16(9), Color::LightRed);
        for i in 16..=255 {
            assert_eq!(rgb_to_256(ansi256_rgb(i)), i, "index {i} maps to itself");
        }
    }

    #[test]
    fn explicit_level_values_win_over_conversion() {
        let spec = ColorSpec {
            rgb: Some((0xe6, 0x9f, 0x00)),
            ansi256: Some(25),
            ansi16: Some(Color::Magenta),
        };
        assert_eq!(
            spec.resolve(ColorLevel::TrueColor),
            Some(Color::Rgb(0xe6, 0x9f, 0x00))
        );
        assert_eq!(spec.resolve(ColorLevel::Ansi256), Some(Color::Indexed(25)));
        assert_eq!(spec.resolve(ColorLevel::Ansi16), Some(Color::Magenta));
        assert_eq!(spec.resolve(ColorLevel::None), None);

        let rgb_only = ColorSpec {
            rgb: Some((0xe6, 0x9f, 0x00)),
            ..ColorSpec::NONE
        };
        assert_eq!(
            rgb_only.resolve(ColorLevel::Ansi256),
            Some(Color::Indexed(178))
        );
        assert_eq!(rgb_only.resolve(ColorLevel::Ansi16), Some(Color::Yellow));

        let indexed = ColorSpec {
            ansi256: Some(25),
            ..ColorSpec::NONE
        };
        assert_eq!(
            indexed.resolve(ColorLevel::TrueColor),
            Some(Color::Indexed(25))
        );
        assert_eq!(indexed.resolve(ColorLevel::Ansi16), Some(Color::Blue));

        let named = ColorSpec::named(Color::Cyan);
        for level in [
            ColorLevel::Ansi16,
            ColorLevel::Ansi256,
            ColorLevel::TrueColor,
        ] {
            assert_eq!(named.resolve(level), Some(Color::Cyan), "{level:?}");
        }
        assert_eq!(ColorSpec::NONE.resolve(ColorLevel::TrueColor), None);
    }

    #[test]
    fn contrast_and_lightness_match_reference_values() {
        assert!((contrast((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 1e-9);
        assert!((contrast((0x77, 0x77, 0x77), (0x77, 0x77, 0x77)) - 1.0).abs() < 1e-9);
        assert!(lightness((0, 0, 0)).abs() < 1e-9);
        assert!((lightness((255, 255, 255)) - 1.0).abs() < 1e-9);
        assert!(lightness((0x76, 0x76, 0x76)) < 0.5);
        assert!(lightness((0x77, 0x77, 0x77)) > 0.5);
    }
}
