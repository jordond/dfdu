//! The built-in themes.
//!
//! `dark` and `light` give truecolor values with explicit 16-colour fallbacks and leave the
//! background to the terminal; `dark` reads on backgrounds from #000000 to #2e3440, `light` on
//! #ffffff to #fdf6e3. Category colours follow the Okabe-Ito palette, lightened on `dark` and
//! darkened on `light` for text contrast. `ansi` uses only the 16 named colours and never black
//! or white text, so it reads on any background. `high-contrast` is `ansi` in bold without dim
//! text. `mono` has no colours, only modifiers.

use ratatui::style::Color::{
    Black, Blue, Cyan, DarkGray, Gray, Green, LightYellow, Magenta, Red, Reset, White, Yellow,
};
use ratatui::style::{Color, Modifier};

use super::Token::{
    BarEmpty, BarFill, Bg, Border, BorderFocus, CatBuild, CatCache, CatContainer, CatDownload,
    CatMedia, CatOther, CatToolchain, CatVm, Cursor, Dir, Fg, FgDim, Header, Key, Mark, Match,
    SafetyKeep, SafetyReview, SafetySafe, ScanActive, ScanDone, SizeBig, StatusError, StatusOk,
    StatusWarn, Title,
};
use super::{ColorSpec, Palette, Spec, Token};

/// A built-in theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTheme {
    Dark,
    Light,
    Ansi,
    Mono,
    HighContrast,
}

impl BuiltinTheme {
    pub const ALL: [BuiltinTheme; 5] = [
        BuiltinTheme::Dark,
        BuiltinTheme::Light,
        BuiltinTheme::Ansi,
        BuiltinTheme::Mono,
        BuiltinTheme::HighContrast,
    ];

    /// Its name in `--theme`, theme files and the palette.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BuiltinTheme::Dark => "dark",
            BuiltinTheme::Light => "light",
            BuiltinTheme::Ansi => "ansi",
            BuiltinTheme::Mono => "mono",
            BuiltinTheme::HighContrast => "high-contrast",
        }
    }

    /// The built-in theme called `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<BuiltinTheme> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    /// Its palette.
    pub(crate) fn palette(self) -> Palette {
        let mut p: Palette = [Spec::NONE; Token::COUNT];
        for (t, spec) in entries(self) {
            p[t as usize] = spec;
        }
        p
    }
}

/// The names of [`BuiltinTheme::ALL`].
pub const NAMES: [&str; 5] = [
    BuiltinTheme::Dark.name(),
    BuiltinTheme::Light.name(),
    BuiltinTheme::Ansi.name(),
    BuiltinTheme::Mono.name(),
    BuiltinTheme::HighContrast.name(),
];

const BOLD: Modifier = Modifier::BOLD;

fn entries(theme: BuiltinTheme) -> [(Token, Spec); Token::COUNT] {
    match theme {
        BuiltinTheme::Dark => dark(),
        BuiltinTheme::Light => light(),
        BuiltinTheme::Ansi => ansi(),
        BuiltinTheme::Mono => mono(),
        BuiltinTheme::HighContrast => high_contrast(),
    }
}

/// A truecolor value with its 16-colour fallback.
const fn rgb(hex: u32, ansi16: Color) -> ColorSpec {
    let [_, r, g, b] = hex.to_be_bytes();
    ColorSpec {
        rgb: Some((r, g, b)),
        ansi256: None,
        ansi16: Some(ansi16),
    }
}

const fn named(c: Color) -> ColorSpec {
    ColorSpec::named(c)
}

const fn fg(c: ColorSpec) -> Spec {
    Spec::fg(c)
}

const fn modifiers(m: Modifier) -> Spec {
    Spec::NONE.add(m)
}

#[expect(
    clippy::unreadable_literal,
    reason = "colours are written as six-digit #rrggbb hex codes"
)]
fn dark() -> [(Token, Spec); Token::COUNT] {
    let text = rgb(0xeceff4, Reset);
    let accent = rgb(0x88c0d0, Cyan);
    let blue = ColorSpec {
        ansi256: Some(69),
        ..rgb(0x7aa2f7, Blue)
    };
    let orange = rgb(0xe69f00, Yellow);
    let vermillion = rgb(0xf08040, Red);
    let green = rgb(0xa3be8c, Green);
    [
        (Fg, fg(rgb(0xd8dee9, Reset))),
        (FgDim, fg(rgb(0x8b94a7, DarkGray))),
        (Bg, Spec::NONE.bg(named(Reset))),
        (Border, fg(rgb(0x7b8394, DarkGray))),
        (BorderFocus, fg(accent)),
        (Header, fg(text).add(BOLD)),
        (Title, fg(text).add(BOLD)),
        (
            Cursor,
            fg(rgb(0xeceff4, White))
                .bg(rgb(0x4c566a, DarkGray))
                .add(BOLD),
        ),
        (Mark, fg(rgb(0xd8a6e0, Magenta)).add(BOLD)),
        (Dir, fg(rgb(0x8fbcf0, Blue)).add(BOLD)),
        (Key, fg(accent).add(BOLD)),
        (BarFill, fg(accent)),
        (BarEmpty, fg(rgb(0x434c5e, DarkGray))),
        (SizeBig, fg(text).add(BOLD)),
        (ScanActive, fg(accent)),
        (ScanDone, fg(green)),
        (StatusError, fg(rgb(0xff8f8f, Red))),
        (StatusWarn, fg(rgb(0xebcb8b, Yellow))),
        (StatusOk, fg(green)),
        (Match, fg(rgb(0xf0c674, Yellow)).add(Modifier::UNDERLINED)),
        (CatCache, fg(orange)),
        (CatBuild, fg(blue)),
        (CatVm, fg(rgb(0xdc8cb8, Magenta))),
        (CatToolchain, fg(rgb(0x1fbf8f, Green))),
        (CatMedia, fg(rgb(0x56b4e9, Cyan))),
        (CatDownload, fg(vermillion)),
        (CatContainer, fg(rgb(0xf0e442, LightYellow))),
        (CatOther, fg(rgb(0xa8a8a8, DarkGray))),
        (SafetySafe, fg(blue)),
        (SafetyReview, fg(orange)),
        (SafetyKeep, fg(vermillion).add(BOLD)),
    ]
}

#[expect(
    clippy::unreadable_literal,
    reason = "colours are written as six-digit #rrggbb hex codes"
)]
fn light() -> [(Token, Spec); Token::COUNT] {
    let text = rgb(0x1f2329, Reset);
    let accent = rgb(0x1b6b8c, Blue);
    let blue = rgb(0x0068a6, Blue);
    let orange = rgb(0x9a5f00, Yellow);
    let vermillion = rgb(0xb24700, Red);
    let green = rgb(0x2e7d32, Green);
    [
        (Fg, fg(rgb(0x2e3440, Reset))),
        (FgDim, fg(rgb(0x6c7180, DarkGray))),
        (Bg, Spec::NONE.bg(named(Reset))),
        (Border, fg(rgb(0x80868f, DarkGray))),
        (BorderFocus, fg(rgb(0x2a6f97, Blue))),
        (Header, fg(text).add(BOLD)),
        (Title, fg(text).add(BOLD)),
        (
            Cursor,
            fg(rgb(0x1f2329, Black)).bg(rgb(0xd5dce8, Gray)).add(BOLD),
        ),
        (Mark, fg(rgb(0x8e3a9e, Magenta)).add(BOLD)),
        (Dir, fg(rgb(0x1d5ea8, Blue)).add(BOLD)),
        (Key, fg(accent).add(BOLD)),
        (BarFill, fg(rgb(0x3d6fa8, Blue))),
        (BarEmpty, fg(rgb(0xe2dccb, Gray))),
        (SizeBig, fg(text).add(BOLD)),
        (ScanActive, fg(accent)),
        (ScanDone, fg(green)),
        (StatusError, fg(rgb(0xb3261e, Red))),
        (StatusWarn, fg(rgb(0x8a5300, Yellow))),
        (StatusOk, fg(green)),
        (Match, fg(rgb(0x9a3c00, Magenta)).add(Modifier::UNDERLINED)),
        (CatCache, fg(orange)),
        (CatBuild, fg(blue)),
        (CatVm, fg(rgb(0xa3407a, Magenta))),
        (CatToolchain, fg(rgb(0x007858, Green))),
        (CatMedia, fg(rgb(0x00718f, Cyan))),
        (CatDownload, fg(vermillion)),
        (CatContainer, fg(rgb(0x766a00, Yellow))),
        (CatOther, fg(rgb(0x666666, DarkGray))),
        (SafetySafe, fg(blue)),
        (SafetyReview, fg(orange)),
        (SafetyKeep, fg(vermillion).add(BOLD)),
    ]
}

fn ansi() -> [(Token, Spec); Token::COUNT] {
    [
        (Fg, Spec::NONE),
        (FgDim, fg(named(DarkGray))),
        (Bg, Spec::NONE.bg(named(Reset))),
        (Border, Spec::NONE),
        (BorderFocus, fg(named(Cyan))),
        (Header, modifiers(BOLD)),
        (Title, modifiers(BOLD)),
        (Cursor, modifiers(Modifier::REVERSED)),
        (Mark, fg(named(Magenta)).add(BOLD)),
        (Dir, fg(named(Blue)).add(BOLD)),
        (Key, modifiers(BOLD)),
        (BarFill, fg(named(Cyan))),
        (BarEmpty, Spec::NONE),
        (SizeBig, modifiers(BOLD)),
        (ScanActive, fg(named(Cyan))),
        (ScanDone, fg(named(Green))),
        (StatusError, fg(named(Red))),
        (StatusWarn, fg(named(Yellow))),
        (StatusOk, fg(named(Green))),
        (Match, fg(named(Cyan)).add(Modifier::UNDERLINED)),
        (CatCache, fg(named(Yellow))),
        (CatBuild, fg(named(Blue))),
        (CatVm, fg(named(Magenta))),
        (CatToolchain, fg(named(Green))),
        (CatMedia, fg(named(Cyan))),
        (CatDownload, fg(named(Red))),
        (CatContainer, fg(named(LightYellow))),
        (CatOther, Spec::NONE),
        (SafetySafe, fg(named(Blue))),
        (SafetyReview, fg(named(Yellow))),
        (SafetyKeep, fg(named(Red)).add(BOLD)),
    ]
}

fn high_contrast() -> [(Token, Spec); Token::COUNT] {
    ansi().map(|(t, spec)| {
        let spec = match t {
            FgDim => Spec::NONE,
            Bg => spec,
            _ => spec.add(BOLD),
        };
        (t, spec)
    })
}

fn mono() -> [(Token, Spec); Token::COUNT] {
    Token::ALL.map(|t| {
        let m = match t {
            FgDim => Modifier::DIM,
            Cursor => Modifier::REVERSED,
            Match => Modifier::UNDERLINED,
            Header | Title | Mark | Dir | Key | SizeBig | StatusError | StatusWarn
            | BorderFocus | SafetyKeep => BOLD,
            _ => Modifier::empty(),
        };
        (t, modifiers(m))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::style::Style;

    use super::*;
    use crate::theme::color::contrast;
    use crate::theme::{Category, ColorLevel, Rgb, Theme};

    /// The lightest background `dark` is meant for.
    const DARK_BACKGROUND: Rgb = (0x2e, 0x34, 0x40);
    /// The darkest background `light` is meant for.
    const LIGHT_BACKGROUND: Rgb = (0xfd, 0xf6, 0xe3);

    fn rgb_of(c: Option<Color>, what: &str) -> Rgb {
        match c {
            Some(Color::Rgb(r, g, b)) => (r, g, b),
            other => panic!("{what}: expected an RGB colour, got {other:?}"),
        }
    }

    fn colors(s: Style) -> [Option<Color>; 2] {
        [s.fg, s.bg]
    }

    #[test]
    fn builtin_palettes_set_every_token_once() {
        for b in BuiltinTheme::ALL {
            let tokens: HashSet<Token> = entries(b).map(|(t, _)| t).into();
            assert_eq!(tokens.len(), Token::COUNT, "{b:?}");
            assert_eq!(BuiltinTheme::parse(b.name()), Some(b));
        }
        assert_eq!(BuiltinTheme::parse("solarized"), None);
    }

    #[test]
    fn contrast_meets_wcag_for_text_tokens() {
        const UI: [Token; 4] = [FgDim, BarFill, Border, BorderFocus];
        for (name, bg) in [("dark", DARK_BACKGROUND), ("light", LIGHT_BACKGROUND)] {
            let t = Theme::named(name, ColorLevel::TrueColor, None).expect(name);
            for token in Token::ALL {
                let s = t.style(token);
                let need = match token {
                    Bg | BarEmpty => continue,
                    Cursor => {
                        let ratio = contrast(rgb_of(s.fg, "cursor fg"), rgb_of(s.bg, "cursor bg"));
                        assert!(ratio >= 4.5, "{name} cursor: {ratio:.2}");
                        continue;
                    }
                    t if UI.contains(&t) => 3.0,
                    _ => 4.5,
                };
                let ratio = contrast(rgb_of(s.fg, token.name()), bg);
                assert!(
                    ratio >= need,
                    "{name} {}: {ratio:.2} < {need}",
                    token.name()
                );
            }
        }
    }

    #[test]
    fn ansi_theme_never_uses_black_or_white_text() {
        let named = [
            Reset,
            Black,
            Red,
            Green,
            Yellow,
            Blue,
            Magenta,
            Cyan,
            Gray,
            DarkGray,
            Color::LightRed,
            Color::LightGreen,
            LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            White,
        ];
        for level in [
            ColorLevel::Ansi16,
            ColorLevel::Ansi256,
            ColorLevel::TrueColor,
        ] {
            for name in ["ansi", "high-contrast"] {
                let t = Theme::named(name, level, None).expect(name);
                for token in Token::ALL {
                    let s = t.style(token);
                    for c in colors(s).into_iter().flatten() {
                        assert!(named.contains(&c), "{name} {}: {c:?}", token.name());
                    }
                    match s.fg {
                        Some(Black | White | Gray) => {
                            panic!("{name} {} uses {:?} text", token.name(), s.fg)
                        }
                        Some(DarkGray) => assert_eq!(token, FgDim, "{name}"),
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn mono_theme_has_no_colors() {
        for level in [ColorLevel::None, ColorLevel::TrueColor] {
            let t = Theme::named("mono", level, None).expect("mono");
            for token in Token::ALL {
                for c in colors(t.style(token)) {
                    assert!(matches!(c, None | Some(Reset)), "{}: {c:?}", token.name());
                }
            }
            assert_eq!(t.cursor, Style::new().add_modifier(Modifier::REVERSED));
            assert_eq!(t.mark, Style::new().add_modifier(BOLD));
            assert_eq!(t.size_big, Style::new().add_modifier(BOLD));
            assert_eq!(t.fg_dim, Style::new().add_modifier(Modifier::DIM));
            assert_eq!(t.r#match, Style::new().add_modifier(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn every_level_resolves_to_its_palette() {
        let allowed = |level, c: Color| match level {
            ColorLevel::Ansi16 => !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
            ColorLevel::Ansi256 => !matches!(c, Color::Rgb(..)),
            _ => false,
        };
        for level in [ColorLevel::Ansi16, ColorLevel::Ansi256, ColorLevel::None] {
            let t = Theme::named("dark", level, None).expect("dark");
            for token in Token::ALL {
                for c in colors(t.style(token)).into_iter().flatten() {
                    assert!(allowed(level, c), "{level:?} {}: {c:?}", token.name());
                }
            }
        }
        let indexed = Theme::named("dark", ColorLevel::Ansi256, None).expect("dark");
        assert_eq!(indexed.fg.fg, Some(Color::Indexed(253)));
        assert_eq!(
            indexed.cat[Category::Build as usize].fg,
            Some(Color::Indexed(69))
        );
        let none = Theme::named("dark", ColorLevel::None, None).expect("dark");
        assert!(none.cursor.add_modifier.contains(Modifier::REVERSED));
        assert!(none.r#match.add_modifier.contains(Modifier::UNDERLINED));
        assert!(none.fg_dim.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn high_contrast_is_bold_without_dim() {
        for level in [ColorLevel::Ansi16, ColorLevel::None] {
            let t = Theme::named("high-contrast", level, None).expect("theme");
            for token in Token::ALL {
                let s = t.style(token);
                assert!(
                    !s.add_modifier.contains(Modifier::DIM),
                    "{level:?} {}",
                    token.name()
                );
                if !matches!(token, Bg | FgDim) {
                    assert!(s.add_modifier.contains(BOLD), "{level:?} {}", token.name());
                }
            }
            assert_eq!(t.fg_dim.fg, None);
            assert!(t.cursor.add_modifier.contains(Modifier::REVERSED));
        }
    }
}
