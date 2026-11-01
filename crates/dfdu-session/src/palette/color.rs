//! Colours with a value per colour level, the 16 named ANSI colours, and text modifiers.

use std::ops::{BitOr, BitOrAssign};

/// An sRGB colour with 8 bits per channel.
pub type Rgb = (u8, u8, u8);

/// One of the 16 named ANSI colours, which a terminal shows in its own palette, or `Reset`, the
/// terminal's default colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnsiColor {
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
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

impl AnsiColor {
    /// The colour called `name`, spelled as ratatui accepts it: case, spaces, `-` and `_` do
    /// not matter, `bright` is `light`, `grey` and `silver` are `gray`, `light black` is
    /// `dark gray`, and `light white` and `light gray` are `white`.
    #[must_use]
    pub fn parse(name: &str) -> Option<AnsiColor> {
        let name = name
            .to_lowercase()
            .replace([' ', '-', '_'], "")
            .replace("bright", "light")
            .replace("grey", "gray")
            .replace("silver", "gray")
            .replace("lightblack", "darkgray")
            .replace("lightwhite", "white")
            .replace("lightgray", "white");
        Some(match name.as_str() {
            "reset" => AnsiColor::Reset,
            "black" => AnsiColor::Black,
            "red" => AnsiColor::Red,
            "green" => AnsiColor::Green,
            "yellow" => AnsiColor::Yellow,
            "blue" => AnsiColor::Blue,
            "magenta" => AnsiColor::Magenta,
            "cyan" => AnsiColor::Cyan,
            "gray" => AnsiColor::Gray,
            "darkgray" => AnsiColor::DarkGray,
            "lightred" => AnsiColor::LightRed,
            "lightgreen" => AnsiColor::LightGreen,
            "lightyellow" => AnsiColor::LightYellow,
            "lightblue" => AnsiColor::LightBlue,
            "lightmagenta" => AnsiColor::LightMagenta,
            "lightcyan" => AnsiColor::LightCyan,
            "white" => AnsiColor::White,
            _ => return None,
        })
    }
}

/// A colour with optional explicit values for each colour level. A terminal uses the value for
/// its level when given and otherwise converts from the richest value there is (a colour with
/// only poorer values uses those unchanged: a named ANSI colour stays named at every level). A
/// front end with full colour uses `rgb`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorSpec {
    pub rgb: Option<Rgb>,
    pub ansi256: Option<u8>,
    pub ansi16: Option<AnsiColor>,
}

impl ColorSpec {
    /// No colour at any level: the cell keeps whatever colour it had.
    pub const NONE: ColorSpec = ColorSpec {
        rgb: None,
        ansi256: None,
        ansi16: None,
    };

    /// A named ANSI colour, used at every level.
    #[must_use]
    pub const fn named(c: AnsiColor) -> Self {
        ColorSpec {
            ansi16: Some(c),
            ..Self::NONE
        }
    }
}

/// A set of text modifiers. A front end shows the ones it can and ignores the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(u16);

impl Modifiers {
    pub const BOLD: Modifiers = Modifiers(1);
    pub const DIM: Modifiers = Modifiers(1 << 1);
    pub const ITALIC: Modifiers = Modifiers(1 << 2);
    pub const UNDERLINED: Modifiers = Modifiers(1 << 3);
    pub const SLOW_BLINK: Modifiers = Modifiers(1 << 4);
    pub const RAPID_BLINK: Modifiers = Modifiers(1 << 5);
    pub const REVERSED: Modifiers = Modifiers(1 << 6);
    pub const HIDDEN: Modifiers = Modifiers(1 << 7);
    pub const CROSSED_OUT: Modifiers = Modifiers(1 << 8);

    /// Every modifier, one at a time.
    pub const ALL: [Modifiers; 9] = [
        Self::BOLD,
        Self::DIM,
        Self::ITALIC,
        Self::UNDERLINED,
        Self::SLOW_BLINK,
        Self::RAPID_BLINK,
        Self::REVERSED,
        Self::HIDDEN,
        Self::CROSSED_OUT,
    ];

    #[must_use]
    pub const fn empty() -> Self {
        Modifiers(0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn union(self, other: Modifiers) -> Self {
        Modifiers(self.0 | other.0)
    }

    /// Whether every modifier of `other` is in `self`.
    #[must_use]
    pub const fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    /// The modifier called `name` in a theme file: `bold`, `dim`, `italic`, `underlined`
    /// (`underline`), `slow_blink`, `rapid_blink`, `reversed` (`reverse`), `hidden` or
    /// `crossed_out` (`strikethrough`); case does not matter and `-` is `_`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Modifiers> {
        Some(match name.to_ascii_lowercase().replace('-', "_").as_str() {
            "bold" => Self::BOLD,
            "dim" => Self::DIM,
            "italic" => Self::ITALIC,
            "underlined" | "underline" => Self::UNDERLINED,
            "slow_blink" => Self::SLOW_BLINK,
            "rapid_blink" => Self::RAPID_BLINK,
            "reversed" | "reverse" => Self::REVERSED,
            "hidden" => Self::HIDDEN,
            "crossed_out" | "strikethrough" => Self::CROSSED_OUT,
            _ => return None,
        })
    }
}

impl BitOr for Modifiers {
    type Output = Modifiers;

    fn bitor(self, rhs: Modifiers) -> Modifiers {
        self.union(rhs)
    }
}

impl BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Modifiers) {
        *self = self.union(rhs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_names_parse_as_ratatui_spells_them() {
        let cases = [
            ("yellow", Some(AnsiColor::Yellow)),
            ("Bright Yellow", Some(AnsiColor::LightYellow)),
            ("light-yellow", Some(AnsiColor::LightYellow)),
            ("dark_gray", Some(AnsiColor::DarkGray)),
            ("bright black", Some(AnsiColor::DarkGray)),
            ("grey", Some(AnsiColor::Gray)),
            ("silver", Some(AnsiColor::Gray)),
            ("light gray", Some(AnsiColor::White)),
            ("bright white", Some(AnsiColor::White)),
            ("RESET", Some(AnsiColor::Reset)),
            ("purple", None),
            ("12", None),
            ("#ff0000", None),
        ];
        for (name, want) in cases {
            assert_eq!(AnsiColor::parse(name), want, "{name}");
        }
    }

    #[test]
    fn modifiers_combine_and_parse() {
        let m = Modifiers::BOLD | Modifiers::UNDERLINED;
        assert!(m.contains(Modifiers::BOLD) && m.contains(Modifiers::UNDERLINED));
        assert!(!m.contains(Modifiers::BOLD | Modifiers::DIM));
        assert!(Modifiers::empty().is_empty());
        for (i, a) in Modifiers::ALL.into_iter().enumerate() {
            for b in &Modifiers::ALL[i + 1..] {
                assert!(!a.contains(*b), "{a:?} and {b:?} are distinct");
            }
        }
        assert_eq!(Modifiers::parse("Strike-Through"), None);
        assert_eq!(
            Modifiers::parse("CROSSED-OUT"),
            Some(Modifiers::CROSSED_OUT)
        );
        assert_eq!(Modifiers::parse("reverse"), Some(Modifiers::REVERSED));
        assert_eq!(Modifiers::parse("blinking"), None);
    }
}
