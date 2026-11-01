//! Tests of the built-in themes (`dfdu_session::palette::BuiltinTheme`) resolved to styles for
//! each colour level.

use ratatui::style::Color::{
    Black, Blue, Cyan, DarkGray, Gray, Green, LightYellow, Magenta, Red, Reset, White, Yellow,
};
use ratatui::style::{Color, Modifier, Style};

use super::Token::{BarEmpty, BarFill, Bg, Border, BorderFocus, Cursor, FgDim};
use super::color::contrast;
use super::{Category, ColorLevel, Rgb, Theme, Token};

const BOLD: Modifier = Modifier::BOLD;

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
