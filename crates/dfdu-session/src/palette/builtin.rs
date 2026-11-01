//! The built-in themes.
//!
//! `dark` and `light` give truecolor values with explicit 16-colour fallbacks and leave the
//! background to the terminal; `dark` reads on backgrounds from #000000 to #2e3440, `light` on
//! #ffffff to #fdf6e3. Category colours follow the Okabe-Ito palette, lightened on `dark` and
//! darkened on `light` for text contrast. `ansi` uses only the 16 named colours and never black
//! or white text, so it reads on any background. `high-contrast` is `ansi` in bold without dim
//! text. `mono` has no colours, only modifiers.

use super::AnsiColor::{
    Black, Blue, Cyan, DarkGray, Gray, Green, LightYellow, Magenta, Red, Reset, White, Yellow,
};
use super::Token::{
    BarEmpty, BarFill, Bg, Border, BorderFocus, CatBuild, CatCache, CatContainer, CatDownload,
    CatMedia, CatOther, CatToolchain, CatVm, Cursor, Dir, Fg, FgDim, Header, Key, Mark, Match,
    SafetyKeep, SafetyReview, SafetySafe, ScanActive, ScanDone, SizeBig, StatusError, StatusOk,
    StatusWarn, Title,
};
use super::{AnsiColor, ColorSpec, Modifiers, Palette, Spec, Token};

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

    /// The names of [`BuiltinTheme::ALL`].
    pub const NAMES: [&'static str; 5] = [
        BuiltinTheme::Dark.name(),
        BuiltinTheme::Light.name(),
        BuiltinTheme::Ansi.name(),
        BuiltinTheme::Mono.name(),
        BuiltinTheme::HighContrast.name(),
    ];

    /// The built-in theme called `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<BuiltinTheme> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    /// Its palette.
    #[must_use]
    pub fn palette(self) -> Palette {
        let mut p = Palette::NONE;
        for (t, spec) in entries(self) {
            p[t] = spec;
        }
        p
    }
}

const BOLD: Modifiers = Modifiers::BOLD;

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
const fn rgb(hex: u32, ansi16: AnsiColor) -> ColorSpec {
    let [_, r, g, b] = hex.to_be_bytes();
    ColorSpec {
        rgb: Some((r, g, b)),
        ansi256: None,
        ansi16: Some(ansi16),
    }
}

const fn named(c: AnsiColor) -> ColorSpec {
    ColorSpec::named(c)
}

const fn fg(c: ColorSpec) -> Spec {
    Spec::fg(c)
}

const fn modifiers(m: Modifiers) -> Spec {
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
        (Match, fg(rgb(0xf0c674, Yellow)).add(Modifiers::UNDERLINED)),
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
        (Match, fg(rgb(0x9a3c00, Magenta)).add(Modifiers::UNDERLINED)),
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
        (Cursor, modifiers(Modifiers::REVERSED)),
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
        (Match, fg(named(Cyan)).add(Modifiers::UNDERLINED)),
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
            FgDim => Modifiers::DIM,
            Cursor => Modifiers::REVERSED,
            Match => Modifiers::UNDERLINED,
            Header | Title | Mark | Dir | Key | SizeBig | StatusError | StatusWarn
            | BorderFocus | SafetyKeep => BOLD,
            _ => Modifiers::empty(),
        };
        (t, modifiers(m))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

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
    fn builtin_palettes_hold_their_values() {
        let dark = BuiltinTheme::Dark.palette();
        assert_eq!(dark[Fg].fg.rgb, Some((0xd8, 0xde, 0xe9)));
        assert_eq!(dark[CatBuild].fg.ansi256, Some(69));
        assert_eq!(dark[Cursor].bg.ansi16, Some(DarkGray));
        assert_eq!(dark[Match].modifiers, Modifiers::UNDERLINED);
        let ansi = BuiltinTheme::Ansi.palette();
        assert_eq!(ansi[Dir], Spec::fg(named(Blue)).add(BOLD));
        assert_eq!(ansi[Fg], Spec::NONE);
        let mono = BuiltinTheme::Mono.palette();
        for t in Token::ALL {
            assert_eq!((mono[t].fg, mono[t].bg), (ColorSpec::NONE, ColorSpec::NONE));
        }
        assert_eq!(mono[Cursor].modifiers, Modifiers::REVERSED);
    }
}
