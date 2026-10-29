//! Colour-level detection, the choice of a built-in theme, and the background-colour query.
//! Detection and choice are pure functions of the flags and an environment lookup.

use std::ffi::{OsStr, OsString};
use std::time::Duration;

use super::color::lightness;
use super::{BuiltinTheme, ColorLevel, Rgb};

/// The detected colour level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorDetection {
    pub level: ColorLevel,
    /// A `--color` flag asked for colour although `NO_COLOR` is set; crossterm then needs
    /// `force_color_output(true)`, or it drops the colours itself.
    pub overrides_no_color: bool,
}

/// Parses a `--color` value: `auto` (detect), `never`, `16`, `256` or `truecolor`.
///
/// # Errors
///
/// For any other value, with a message for the command line that lists the valid ones.
pub fn parse_color_flag(s: &str) -> Result<Option<ColorLevel>, String> {
    Ok(Some(match s {
        "auto" => return Ok(None),
        "never" => ColorLevel::None,
        "16" => ColorLevel::Ansi16,
        "256" => ColorLevel::Ansi256,
        "truecolor" => ColorLevel::TrueColor,
        _ => {
            return Err(format!(
                "expected auto, never, 16, 256 or truecolor, not {s:?}"
            ));
        }
    }))
}

/// The colour level from the `--color` flag (`None` for `auto`) and the environment, first
/// match wins: the flag; `NO_COLOR` not empty (no colour); `FORCE_COLOR` or `CLICOLOR_FORCE`
/// (at least 256 colours, more if the rules below say so); `COLORTERM` `truecolor` or `24bit`
/// (case-sensitive); `WT_SESSION` set (Windows Terminal, truecolor); `TERM` containing
/// `256color`; `TERM` `linux` or `vt*` (16 colours); `TERM=dumb` (no colour); otherwise 256.
pub fn detect_color_level(
    flag: Option<ColorLevel>,
    env: impl Fn(&str) -> Option<OsString>,
) -> ColorDetection {
    let no_color = env("NO_COLOR").is_some_and(|v| !v.is_empty());
    if let Some(level) = flag {
        return ColorDetection {
            level,
            overrides_no_color: no_color && level != ColorLevel::None,
        };
    }
    let level = if no_color {
        ColorLevel::None
    } else {
        let level = terminal_level(&env);
        if forced(env("FORCE_COLOR")) || forced(env("CLICOLOR_FORCE")) {
            level.max(ColorLevel::Ansi256)
        } else {
            level
        }
    };
    ColorDetection {
        level,
        overrides_no_color: false,
    }
}

/// A force variable counts when set to anything but empty, `0` or `false`.
fn forced(v: Option<OsString>) -> bool {
    v.is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
}

fn terminal_level(env: &impl Fn(&str) -> Option<OsString>) -> ColorLevel {
    let colorterm = env("COLORTERM");
    if matches!(
        colorterm.as_deref().and_then(OsStr::to_str),
        Some("truecolor" | "24bit")
    ) {
        return ColorLevel::TrueColor;
    }
    if env("WT_SESSION").is_some() {
        return ColorLevel::TrueColor;
    }
    let term = env("TERM");
    let term = term.as_deref().and_then(OsStr::to_str).unwrap_or("");
    if term.contains("256color") {
        ColorLevel::Ansi256
    } else if term == "linux" || term.starts_with("vt") {
        ColorLevel::Ansi16
    } else if term == "dumb" {
        ColorLevel::None
    } else {
        ColorLevel::Ansi256
    }
}

/// The theme to use: the one named explicitly; `mono` without colours; `ansi` at 16 colours or
/// when the background is unknown; otherwise `dark` or `light` by the background's perceived
/// lightness.
pub fn choose_theme(
    explicit: Option<BuiltinTheme>,
    level: ColorLevel,
    background: Option<Rgb>,
) -> BuiltinTheme {
    if let Some(b) = explicit {
        return b;
    }
    match (level, background) {
        (ColorLevel::None, _) => BuiltinTheme::Mono,
        (ColorLevel::Ansi16, _) | (_, None) => BuiltinTheme::Ansi,
        (_, Some(bg)) if lightness(bg) < 0.5 => BuiltinTheme::Dark,
        _ => BuiltinTheme::Light,
    }
}

/// Whether asking the terminal for its background can change the theme: not when the theme is
/// named explicitly, with fewer than 256 colours, or on a dumb terminal.
pub fn should_query_background(
    explicit: Option<BuiltinTheme>,
    level: ColorLevel,
    env: impl Fn(&str) -> Option<OsString>,
) -> bool {
    explicit.is_none() && level >= ColorLevel::Ansi256 && env("TERM").is_none_or(|t| t != "dumb")
}

/// Asks the terminal for its background colour (OSC 11) and waits at most `timeout` for the
/// answer; `None` on any error, including terminals that do not answer. It switches raw mode on
/// and off itself, so it must run before the alternate screen is entered and before anything
/// else reads input.
pub fn query_background(timeout: Duration) -> Option<Rgb> {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = timeout;
    terminal_colorsaurus::background_color(options)
        .ok()
        .map(|c| c.scale_to_8bit())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use BuiltinTheme::{Ansi, Dark, Light, Mono};
    use ColorLevel::{Ansi16, Ansi256, None as Off, TrueColor};

    type Env<'a> = &'a [(&'a str, &'a str)];

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        let map: HashMap<&str, &str> = vars.iter().copied().collect();
        move |k| map.get(k).map(OsString::from)
    }

    #[test]
    fn color_level_detection_order() {
        let cases: &[(Option<ColorLevel>, Env, ColorLevel, bool)] = &[
            (None, &[], Ansi256, false),
            (Some(Ansi16), &[("COLORTERM", "truecolor")], Ansi16, false),
            (Some(Off), &[("COLORTERM", "truecolor")], Off, false),
            (Some(TrueColor), &[("NO_COLOR", "1")], TrueColor, true),
            (Some(Off), &[("NO_COLOR", "1")], Off, false),
            (
                None,
                &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
                Off,
                false,
            ),
            (None, &[("NO_COLOR", "1"), ("FORCE_COLOR", "1")], Off, false),
            (None, &[("NO_COLOR", ""), ("TERM", "xterm")], Ansi256, false),
            (
                None,
                &[("FORCE_COLOR", "1"), ("TERM", "dumb")],
                Ansi256,
                false,
            ),
            (
                None,
                &[("CLICOLOR_FORCE", "1"), ("TERM", "linux")],
                Ansi256,
                false,
            ),
            (
                None,
                &[("FORCE_COLOR", "0"), ("TERM", "linux")],
                Ansi16,
                false,
            ),
            (
                None,
                &[("FORCE_COLOR", "1"), ("COLORTERM", "24bit")],
                TrueColor,
                false,
            ),
            (
                None,
                &[("COLORTERM", "truecolor"), ("TERM", "dumb")],
                TrueColor,
                false,
            ),
            (None, &[("COLORTERM", "24bit")], TrueColor, false),
            (
                None,
                &[("COLORTERM", "TrueColor"), ("TERM", "linux")],
                Ansi16,
                false,
            ),
            (
                None,
                &[("WT_SESSION", "x"), ("TERM", "vt100")],
                TrueColor,
                false,
            ),
            (None, &[("TERM", "xterm-256color")], Ansi256, false),
            (None, &[("TERM", "screen.xterm-256color")], Ansi256, false),
            (None, &[("TERM", "linux")], Ansi16, false),
            (None, &[("TERM", "vt220")], Ansi16, false),
            (None, &[("TERM", "dumb")], Off, false),
            (None, &[("TERM", "xterm")], Ansi256, false),
        ];
        for &(flag, vars, level, overrides) in cases {
            let got = detect_color_level(flag, env(vars));
            let want = ColorDetection {
                level,
                overrides_no_color: overrides,
            };
            assert_eq!(got, want, "flag {flag:?}, env {vars:?}");
        }
    }

    #[test]
    fn color_flag_values_parse() {
        assert_eq!(parse_color_flag("auto"), Ok(None));
        assert_eq!(parse_color_flag("never"), Ok(Some(Off)));
        assert_eq!(parse_color_flag("16"), Ok(Some(Ansi16)));
        assert_eq!(parse_color_flag("256"), Ok(Some(Ansi256)));
        assert_eq!(parse_color_flag("truecolor"), Ok(Some(TrueColor)));
        assert!(parse_color_flag("always").is_err());
    }

    #[test]
    fn theme_choice_from_background() {
        let black = Some((0, 0, 0));
        let white = Some((255, 255, 255));
        assert_eq!(choose_theme(None, TrueColor, black), Dark);
        assert_eq!(choose_theme(None, TrueColor, white), Light);
        assert_eq!(choose_theme(None, Ansi256, black), Dark);
        assert_eq!(
            choose_theme(None, TrueColor, Some((0x76, 0x76, 0x76))),
            Dark
        );
        assert_eq!(
            choose_theme(None, TrueColor, Some((0x88, 0x88, 0x88))),
            Light
        );
        assert_eq!(
            choose_theme(None, TrueColor, Some((0x2e, 0x34, 0x40))),
            Dark
        );
        assert_eq!(
            choose_theme(None, TrueColor, Some((0xfd, 0xf6, 0xe3))),
            Light
        );
        assert_eq!(choose_theme(None, TrueColor, None), Ansi);
        assert_eq!(choose_theme(None, Ansi256, None), Ansi);
        assert_eq!(choose_theme(Some(Light), TrueColor, black), Light);
        assert_eq!(choose_theme(None, Off, white), Mono);
        assert_eq!(choose_theme(None, Ansi16, white), Ansi);
        assert_eq!(choose_theme(None, Ansi16, black), Ansi);
    }

    #[test]
    fn background_query_is_skipped_when_it_cannot_help() {
        let xterm = [("TERM", "xterm-256color")];
        assert!(should_query_background(None, TrueColor, env(&xterm)));
        assert!(should_query_background(None, Ansi256, env(&[])));
        assert!(!should_query_background(None, Ansi16, env(&[])));
        assert!(!should_query_background(Some(Dark), TrueColor, env(&xterm)));
        let no_color = [("NO_COLOR", "1"), ("TERM", "xterm-256color")];
        let level = detect_color_level(None, env(&no_color)).level;
        assert!(!should_query_background(None, level, env(&no_color)));
        let dumb = [("TERM", "dumb"), ("FORCE_COLOR", "1")];
        let level = detect_color_level(None, env(&dumb)).level;
        assert_eq!(level, Ansi256);
        assert!(!should_query_background(None, level, env(&dumb)));
    }
}
