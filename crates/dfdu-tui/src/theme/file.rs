//! A theme file (`dfdu_session::palette::ThemeFile`) resolved for the terminal: its palette for
//! the colour level, and its `bars` key as the bar style.

use super::{Bars, ColorLevel, Rgb, Theme, ThemeFile};

impl Theme {
    /// The theme `file` gives for `level` on a terminal with background `background`, named after
    /// the file's `name`, or after its base when it has none.
    #[must_use]
    pub fn from_file(file: &ThemeFile, level: ColorLevel, background: Option<Rgb>) -> Theme {
        let name = file.theme_name().to_owned();
        let mut theme = Theme::resolve(name, &file.palette(), level, background);
        theme.bars = file.bars().and_then(Bars::parse).unwrap_or_default();
        theme
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};

    use super::*;
    use crate::theme::{BuiltinTheme, Token};

    const EXAMPLE: &str = r##"
name = "my-theme"
base = "dark"            # inherit unset tokens from a built-in theme

[tokens]
fg          = "#d8dee9"
"cat.cache" = { rgb = "#E69F00", ansi256 = 178, ansi16 = "yellow" }
"cat.build" = { rgb = "#0072B2", ansi256 = 25,  ansi16 = "blue" }
"cursor.bg" = { rgb = "#3b4252", ansi16 = "reset", modifiers = ["reversed"] }
"##;

    #[test]
    fn theme_file_parses_the_documented_example() {
        let file = ThemeFile::parse(EXAMPLE).expect("example parses");
        assert_eq!(file.name.as_deref(), Some("my-theme"));
        assert_eq!(file.base(), BuiltinTheme::Dark);

        let t = Theme::from_file(&file, ColorLevel::TrueColor, None);
        assert_eq!(t.name(), "my-theme");
        assert_eq!(t.fg.fg, Some(Color::Rgb(0xd8, 0xde, 0xe9)));
        assert_eq!(t.cat[0].fg, Some(Color::Rgb(0xe6, 0x9f, 0x00)));
        assert_eq!(t.cat[1].fg, Some(Color::Rgb(0x00, 0x72, 0xb2)));
        assert_eq!(t.cursor.bg, Some(Color::Rgb(0x3b, 0x42, 0x52)));
        assert_eq!(t.cursor.add_modifier, Modifier::REVERSED);
        let dark = Theme::named("dark", ColorLevel::TrueColor, None).expect("dark");
        assert_eq!(t.cursor.fg, dark.cursor.fg, "cursor.fg comes from the base");

        let t = Theme::from_file(&file, ColorLevel::Ansi256, None);
        assert_eq!(t.cat[0].fg, Some(Color::Indexed(178)));
        assert_eq!(t.cat[1].fg, Some(Color::Indexed(25)));
        assert_eq!(t.fg.fg, Some(Color::Indexed(253)));

        let t = Theme::from_file(&file, ColorLevel::Ansi16, None);
        assert_eq!(t.cat[0].fg, Some(Color::Yellow));
        assert_eq!(t.cat[1].fg, Some(Color::Blue));
        assert_eq!(t.cursor.bg, Some(Color::Reset));
        assert_eq!(t.fg.fg, Some(Color::Gray));
    }

    #[test]
    fn theme_file_inherits_from_base() {
        for &base in Theme::builtin_names() {
            let file = ThemeFile::parse(&format!("base = \"{base}\"")).expect(base);
            let want = Theme::named(base, ColorLevel::Ansi256, None).expect(base);
            let got = Theme::from_file(&file, ColorLevel::Ansi256, None);
            for t in Token::ALL {
                assert_eq!(got.style(t), want.style(t), "{base} {}", t.name());
            }
            assert_eq!(got.name(), base);
        }

        let file = ThemeFile::parse(
            "base = \"ansi\"\n[tokens]\ndir = \"magenta\"\n\"status.warn\" = { modifiers = [\"bold\"] }\nbg = \"#101010\"",
        )
        .expect("parses");
        let ansi = Theme::ansi16();
        let t = Theme::from_file(&file, ColorLevel::TrueColor, None);
        assert_eq!(t.name(), "ansi");
        assert_eq!(
            t.dir,
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        );
        assert_eq!(t.status_warn, ansi.status_warn.add_modifier(Modifier::BOLD));
        assert_eq!(t.bg, Style::new().bg(Color::Rgb(0x10, 0x10, 0x10)));
        assert_eq!(t.status_error, ansi.status_error);
        assert_eq!(t.title, ansi.title);

        let empty = ThemeFile::parse("").expect("empty file");
        let t = Theme::from_file(&empty, ColorLevel::TrueColor, None);
        assert_eq!(
            t,
            Theme::named("dark", ColorLevel::TrueColor, None).expect("dark")
        );
    }

    #[test]
    fn bars_key_picks_the_bar_style() {
        let t = ThemeFile::parse("bars = \"ascii\"").expect("parses");
        assert_eq!(
            Theme::from_file(&t, ColorLevel::TrueColor, None).bars,
            Bars::Ascii
        );
        let t = ThemeFile::parse("base = \"mono\"").expect("parses");
        assert_eq!(
            Theme::from_file(&t, ColorLevel::None, None).bars,
            Bars::Unicode
        );
        let e = ThemeFile::parse("bars = \"blocks\"").expect_err("bars");
        assert_eq!(e.to_string(), "`bars` must be \"unicode\" or \"ascii\"");
        for name in Bars::NAMES {
            assert!(Bars::parse(name).is_some(), "{name}");
        }
    }
}
