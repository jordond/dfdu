//! Theme files: TOML with a `name`, a built-in `base`, the `bars` style and a `[tokens]` table.
//!
//! ```toml
//! name = "my-theme"
//! base = "dark"            # inherit unset tokens from a built-in theme
//! bars = "ascii"           # or "unicode" (the default); --bars overrides it
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

use toml::{Table, Value};

use super::{AnsiColor, BuiltinTheme, ColorSpec, Modifiers, Palette, Rgb, Token};

/// The values of a theme file's `bars` key: how a terminal draws bars and the treemap's frames.
/// Front ends that do not draw with text ignore it.
pub const BARS: [&str; 2] = ["unicode", "ascii"];

/// A parsed theme file.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeFile {
    pub name: Option<String>,
    base: BuiltinTheme,
    bars: Option<&'static str>,
    tokens: Vec<Setting>,
}

/// One `[tokens]` entry.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Setting {
    token: Token,
    background: bool,
    color: Option<ColorSpec>,
    modifiers: Option<Modifiers>,
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
                BuiltinTheme::NAMES.join(", ")
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
            bars: None,
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
                "bars" => {
                    let bars = string(key, value)?;
                    let bars = BARS.into_iter().find(|&b| b == bars);
                    file.bars =
                        Some(bars.ok_or_else(|| bad_value(key, "\"unicode\" or \"ascii\""))?);
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

    /// The theme's name: the file's `name`, or its base's when it has none.
    #[must_use]
    pub fn theme_name(&self) -> &str {
        self.name.as_deref().unwrap_or(self.base.name())
    }

    /// The file's `bars`, one of [`BARS`], when it has one.
    #[must_use]
    pub fn bars(&self) -> Option<&'static str> {
        self.bars
    }

    /// The base's palette with the file's tokens set.
    #[must_use]
    pub fn palette(&self) -> Palette {
        let mut palette = self.base.palette();
        for s in &self.tokens {
            let spec = &mut palette[s.token];
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
        palette
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

/// One of the 16 ANSI colour names or `reset` ([`AnsiColor::parse`]).
fn named(key: &str, s: &str) -> Result<AnsiColor, ThemeError> {
    AnsiColor::parse(s).ok_or_else(|| bad_color(key, s))
}

fn modifiers(key: &str, value: &Value) -> Result<Modifiers, ThemeError> {
    let list = value
        .as_array()
        .ok_or_else(|| bad_value(key, "an array of modifier names"))?;
    let mut m = Modifiers::empty();
    for v in list {
        let name = string(key, v)?;
        m |= Modifiers::parse(name).ok_or_else(|| ThemeError::UnknownModifier {
            key: key.to_owned(),
            modifier: name.to_owned(),
        })?;
    }
    Ok(m)
}

#[cfg(test)]
mod tests;
