//! Theme files: TOML with a `name`, a built-in `base` and a `[tokens]` table.
//!
//! ```toml
//! name = "my-theme"
//! base = "dark"            # inherit unset tokens from a built-in theme
//!
//! [tokens]
//! fg          = "#d8dee9"
//! "cat.cache" = { rgb = "#E69F00", ansi256 = 178, ansi16 = "yellow" }
//! "cursor.bg" = { rgb = "#3b4252", ansi16 = "reset", modifiers = ["reversed"] }
//! ```
//!
//! A token key is a token name, which sets its foreground (the background for `bg`), or a name
//! followed by `.fg` or `.bg`. A value is a hex colour (converted for 256 and 16 colours), a
//! named ANSI colour (used at every level), or a table with explicit per-level values and
//! `modifiers`, which replace the base's modifiers for that token.

use std::fmt;

use ratatui::style::{Color, Modifier};
use toml::{Table, Value};

use super::{BuiltinTheme, ColorLevel, ColorSpec, Rgb, Theme, Token, builtin};

/// A parsed theme file.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeFile {
    pub name: Option<String>,
    base: BuiltinTheme,
    tokens: Vec<Setting>,
}

/// One `[tokens]` entry.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Setting {
    token: Token,
    background: bool,
    color: Option<ColorSpec>,
    modifiers: Option<Modifier>,
}

/// What is wrong with a theme file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeError {
    /// Not valid TOML.
    Syntax(String),
    UnknownKey(String),
    UnknownToken(String),
    UnknownBase(String),
    BadColor {
        key: String,
        value: String,
    },
    UnknownModifier {
        key: String,
        modifier: String,
    },
    /// `key` has the wrong type or range; `expected` says what it must be.
    BadValue {
        key: String,
        expected: &'static str,
    },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeError::Syntax(e) => write!(f, "{}", e.trim_end()),
            ThemeError::UnknownKey(k) => write!(f, "unknown key `{k}`"),
            ThemeError::UnknownToken(t) => write!(f, "unknown token `{t}`"),
            ThemeError::UnknownBase(b) => write!(
                f,
                "unknown base theme `{b}` (built-in themes: {})",
                builtin::NAMES.join(", ")
            ),
            ThemeError::BadColor { key, value } => write!(
                f,
                "`{key}`: `{value}` is not a colour (expected #rrggbb or an ANSI colour name)"
            ),
            ThemeError::UnknownModifier { key, modifier } => {
                write!(f, "`{key}`: unknown modifier `{modifier}`")
            }
            ThemeError::BadValue { key, expected } => write!(f, "`{key}` must be {expected}"),
        }
    }
}

impl std::error::Error for ThemeError {}

impl ThemeFile {
    /// Reads a theme file's text.
    ///
    /// # Errors
    ///
    /// When the text is not TOML, or has a key, token, colour, modifier or base theme dfdu
    /// does not know, or a value of the wrong type.
    pub fn parse(text: &str) -> Result<ThemeFile, ThemeError> {
        let table: Table = toml::from_str(text).map_err(|e| ThemeError::Syntax(e.to_string()))?;
        let mut file = ThemeFile {
            name: None,
            base: BuiltinTheme::Dark,
            tokens: Vec::new(),
        };
        for (key, value) in &table {
            match key.as_str() {
                "name" => file.name = Some(string(key, value)?.to_owned()),
                "base" => {
                    let base = string(key, value)?;
                    file.base = BuiltinTheme::parse(base)
                        .ok_or_else(|| ThemeError::UnknownBase(base.to_owned()))?;
                }
                "tokens" => {
                    let Value::Table(tokens) = value else {
                        return Err(bad_value(key, "a table"));
                    };
                    for (key, value) in tokens {
                        file.tokens.push(setting(key, value)?);
                    }
                }
                _ => return Err(ThemeError::UnknownKey(key.clone())),
            }
        }
        Ok(file)
    }

    /// The built-in theme the file starts from.
    #[must_use]
    pub fn base(&self) -> BuiltinTheme {
        self.base
    }

    /// The theme for `level` on a terminal with background `background`, named after the file's
    /// `name`, or after its base when it has none.
    #[must_use]
    pub fn resolve(&self, level: ColorLevel, background: Option<Rgb>) -> Theme {
        let mut palette = self.base.palette();
        for s in &self.tokens {
            let spec = &mut palette[s.token as usize];
            if let Some(c) = s.color {
                if s.background {
                    spec.bg = c;
                } else {
                    spec.fg = c;
                }
            }
            if let Some(m) = s.modifiers {
                spec.modifiers = m;
            }
        }
        let name = self
            .name
            .clone()
            .unwrap_or_else(|| self.base.name().to_owned());
        Theme::resolve(name, &palette, level, background)
    }
}

fn setting(key: &str, value: &Value) -> Result<Setting, ThemeError> {
    let (token, background) = if let Some(t) = Token::from_name(key) {
        (t, t == Token::Bg)
    } else {
        let unknown = || ThemeError::UnknownToken(key.to_owned());
        let (name, part) = key.rsplit_once('.').ok_or_else(unknown)?;
        let token = Token::from_name(name).ok_or_else(unknown)?;
        match part {
            "fg" => (token, false),
            "bg" => (token, true),
            _ => return Err(unknown()),
        }
    };
    let mut s = Setting {
        token,
        background,
        color: None,
        modifiers: None,
    };
    match value {
        Value::String(v) => {
            s.color = Some(match hex(v) {
                Some(c) => ColorSpec {
                    rgb: Some(c),
                    ..ColorSpec::NONE
                },
                None => ColorSpec::named(named(key, v)?),
            });
        }
        Value::Table(t) => {
            let mut c = ColorSpec::NONE;
            for (k, v) in t {
                let key = format!("{key}.{k}");
                match k.as_str() {
                    "rgb" => {
                        let v = string(&key, v)?;
                        c.rgb = Some(hex(v).ok_or_else(|| bad_color(&key, v))?);
                    }
                    "ansi256" => {
                        let i = v.as_integer().and_then(|i| u8::try_from(i).ok());
                        c.ansi256 = Some(i.ok_or_else(|| bad_value(&key, "an integer 0 to 255"))?);
                    }
                    "ansi16" => c.ansi16 = Some(named(&key, string(&key, v)?)?),
                    "modifiers" => s.modifiers = Some(modifiers(&key, v)?),
                    _ => return Err(ThemeError::UnknownKey(key)),
                }
            }
            if c != ColorSpec::NONE {
                s.color = Some(c);
            }
        }
        _ => return Err(bad_value(key, "a colour string or a table")),
    }
    Ok(s)
}

fn string<'a>(key: &str, value: &'a Value) -> Result<&'a str, ThemeError> {
    value.as_str().ok_or_else(|| bad_value(key, "a string"))
}

fn bad_value(key: &str, expected: &'static str) -> ThemeError {
    ThemeError::BadValue {
        key: key.to_owned(),
        expected,
    }
}

fn bad_color(key: &str, value: &str) -> ThemeError {
    ThemeError::BadColor {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

/// `#rrggbb`, either case.
fn hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

/// One of the 16 ANSI colour names or `reset`, spelled as ratatui accepts them (`yellow`,
/// `bright yellow`, `light-yellow`, `dark gray`, ...).
fn named(key: &str, s: &str) -> Result<Color, ThemeError> {
    match s.parse::<Color>() {
        Ok(Color::Rgb(..) | Color::Indexed(_)) | Err(_) => Err(bad_color(key, s)),
        Ok(c) => Ok(c),
    }
}

fn modifiers(key: &str, value: &Value) -> Result<Modifier, ThemeError> {
    let list = value
        .as_array()
        .ok_or_else(|| bad_value(key, "an array of modifier names"))?;
    let mut m = Modifier::empty();
    for v in list {
        let name = string(key, v)?;
        m |= match name.to_ascii_lowercase().replace('-', "_").as_str() {
            "bold" => Modifier::BOLD,
            "dim" => Modifier::DIM,
            "italic" => Modifier::ITALIC,
            "underlined" | "underline" => Modifier::UNDERLINED,
            "slow_blink" => Modifier::SLOW_BLINK,
            "rapid_blink" => Modifier::RAPID_BLINK,
            "reversed" | "reverse" => Modifier::REVERSED,
            "hidden" => Modifier::HIDDEN,
            "crossed_out" | "strikethrough" => Modifier::CROSSED_OUT,
            _ => {
                return Err(ThemeError::UnknownModifier {
                    key: key.to_owned(),
                    modifier: name.to_owned(),
                });
            }
        };
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Style;

    use super::*;

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

        let t = file.resolve(ColorLevel::TrueColor, None);
        assert_eq!(t.name(), "my-theme");
        assert_eq!(t.fg.fg, Some(Color::Rgb(0xd8, 0xde, 0xe9)));
        assert_eq!(t.cat[0].fg, Some(Color::Rgb(0xe6, 0x9f, 0x00)));
        assert_eq!(t.cat[1].fg, Some(Color::Rgb(0x00, 0x72, 0xb2)));
        assert_eq!(t.cursor.bg, Some(Color::Rgb(0x3b, 0x42, 0x52)));
        assert_eq!(t.cursor.add_modifier, Modifier::REVERSED);
        let dark = Theme::named("dark", ColorLevel::TrueColor, None).expect("dark");
        assert_eq!(t.cursor.fg, dark.cursor.fg, "cursor.fg comes from the base");

        let t = file.resolve(ColorLevel::Ansi256, None);
        assert_eq!(t.cat[0].fg, Some(Color::Indexed(178)));
        assert_eq!(t.cat[1].fg, Some(Color::Indexed(25)));
        assert_eq!(t.fg.fg, Some(Color::Indexed(253)));

        let t = file.resolve(ColorLevel::Ansi16, None);
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
            let got = file.resolve(ColorLevel::Ansi256, None);
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
        let t = file.resolve(ColorLevel::TrueColor, None);
        assert_eq!(t.name(), "ansi");
        assert_eq!(
            t.dir,
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        );
        assert_eq!(t.status_warn, ansi.status_warn.add_modifier(Modifier::BOLD));
        assert_eq!(t.bg, Style::new().bg(Color::Rgb(0x10, 0x10, 0x10)));
        assert_eq!(t.status_error, ansi.status_error);
        assert_eq!(t.title, ansi.title);

        let t = ThemeFile::parse("")
            .expect("empty file")
            .resolve(ColorLevel::TrueColor, None);
        assert_eq!(
            t,
            Theme::named("dark", ColorLevel::TrueColor, None).expect("dark")
        );
    }

    #[test]
    fn theme_file_errors_name_the_token() {
        let cases: &[(&str, ThemeError, &str)] = &[
            (
                "[tokens]\n\"cat.cahce\" = \"#ffffff\"",
                ThemeError::UnknownToken("cat.cahce".into()),
                "unknown token `cat.cahce`",
            ),
            (
                "[tokens]\n\"dir.underline\" = \"red\"",
                ThemeError::UnknownToken("dir.underline".into()),
                "unknown token `dir.underline`",
            ),
            (
                "[tokens]\ndir = \"#12345\"",
                bad_color("dir", "#12345"),
                "`dir`: `#12345` is not a colour (expected #rrggbb or an ANSI colour name)",
            ),
            (
                "[tokens]\ndir = \"purple\"",
                bad_color("dir", "purple"),
                "`dir`: `purple` is not a colour (expected #rrggbb or an ANSI colour name)",
            ),
            (
                "[tokens]\ndir = { ansi16 = \"#ff0000\" }",
                bad_color("dir.ansi16", "#ff0000"),
                "`dir.ansi16`: `#ff0000` is not a colour (expected #rrggbb or an ANSI colour name)",
            ),
            (
                "[tokens]\ndir = { rgb = \"red\" }",
                bad_color("dir.rgb", "red"),
                "`dir.rgb`: `red` is not a colour (expected #rrggbb or an ANSI colour name)",
            ),
            (
                "[tokens]\nmark = { ansi256 = 300 }",
                bad_value("mark.ansi256", "an integer 0 to 255"),
                "`mark.ansi256` must be an integer 0 to 255",
            ),
            (
                "[tokens]\nmark = { modifiers = [\"bold\", \"blinking\"] }",
                ThemeError::UnknownModifier {
                    key: "mark.modifiers".into(),
                    modifier: "blinking".into(),
                },
                "`mark.modifiers`: unknown modifier `blinking`",
            ),
            (
                "[tokens]\nmark = { colour = \"#ffffff\" }",
                ThemeError::UnknownKey("mark.colour".into()),
                "unknown key `mark.colour`",
            ),
            (
                "[tokens]\nmark = 7",
                bad_value("mark", "a colour string or a table"),
                "`mark` must be a colour string or a table",
            ),
            (
                "tokens = \"none\"",
                bad_value("tokens", "a table"),
                "`tokens` must be a table",
            ),
            (
                "name = 3",
                bad_value("name", "a string"),
                "`name` must be a string",
            ),
            (
                "theme = \"dark\"",
                ThemeError::UnknownKey("theme".into()),
                "unknown key `theme`",
            ),
        ];
        for (text, error, message) in cases {
            let got = ThemeFile::parse(text).expect_err(text);
            assert_eq!(&got, error, "{text}");
            assert_eq!(got.to_string(), *message, "{text}");
        }
        let syntax = ThemeFile::parse("[tokens\nfg = 1").expect_err("syntax");
        assert!(matches!(syntax, ThemeError::Syntax(_)), "{syntax:?}");
    }

    #[test]
    fn unknown_base_is_an_error() {
        let e = ThemeFile::parse("name = \"x\"\nbase = \"solarized\"").expect_err("base");
        assert_eq!(e, ThemeError::UnknownBase("solarized".into()));
        assert_eq!(
            e.to_string(),
            "unknown base theme `solarized` (built-in themes: dark, light, ansi, mono, high-contrast)"
        );
        let e = ThemeFile::parse("base = 1").expect_err("base");
        assert_eq!(e, bad_value("base", "a string"));
    }
}
