use super::*;
use crate::palette::Spec;

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
fn theme_file_palette_sets_the_documented_tokens() {
    let file = ThemeFile::parse(EXAMPLE).expect("example parses");
    assert_eq!(file.theme_name(), "my-theme");
    assert_eq!(file.base(), BuiltinTheme::Dark);
    assert_eq!(file.bars(), None);

    let p = file.palette();
    let dark = BuiltinTheme::Dark.palette();
    let rgb = |c| ColorSpec {
        rgb: Some(c),
        ..ColorSpec::NONE
    };
    assert_eq!(p[Token::Fg], Spec::fg(rgb((0xd8, 0xde, 0xe9))));
    assert_eq!(
        p[Token::CatCache].fg,
        ColorSpec {
            rgb: Some((0xe6, 0x9f, 0x00)),
            ansi256: Some(178),
            ansi16: Some(AnsiColor::Yellow),
        }
    );
    assert_eq!(
        p[Token::Cursor].bg,
        ColorSpec {
            ansi16: Some(AnsiColor::Reset),
            ..rgb((0x3b, 0x42, 0x52))
        }
    );
    assert_eq!(p[Token::Cursor].modifiers, Modifiers::REVERSED);
    assert_eq!(
        p[Token::Cursor].fg,
        dark[Token::Cursor].fg,
        "cursor.fg comes from the base"
    );
    assert_eq!(p[Token::Dir], dark[Token::Dir]);

    let file = ThemeFile::parse("base = \"ansi\"\nbars = \"ascii\"\n[tokens]\nbg = \"magenta\"")
        .expect("parses");
    assert_eq!(file.theme_name(), "ansi");
    assert_eq!(file.bars(), Some("ascii"));
    assert_eq!(
        file.palette()[Token::Bg].bg,
        ColorSpec::named(AnsiColor::Magenta)
    );
    assert_eq!(
        ThemeFile::parse("").expect("empty file").palette(),
        BuiltinTheme::Dark.palette()
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
