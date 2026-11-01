//! The theme's tokens as egui colours and text styles. The built-in dark and light palettes of
//! `dfdu_session::palette` follow the system's setting, or the one chosen; a theme file
//! (`file`) sets the tokens it gives an RGB value over them. The window's title bar follows the
//! theme chosen, or the system's setting ([`title_bar`]). A token without an RGB value (the
//! terminal's own colours) takes egui's colour for text or for the background.

use dfdu_session::palette::{BuiltinTheme, Modifiers, Palette, Rgb, ThemeFile, Token};
use egui::{Color32, RichText, Visuals};

mod file;
pub mod style;

pub use file::{listed, named, preference, saved, title_bar};
pub use style::Chrome;

/// The palette of one of egui's themes, resolved to colours.
#[derive(Clone, Debug)]
pub struct Colors {
    palette: Palette,
    /// egui's text colour, for tokens without a foreground.
    text: Color32,
    /// The window's layers, which the palette has no tokens for.
    pub chrome: Chrome,
}

fn color((r, g, b): Rgb) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Colors {
    /// The built-in palette for `theme`, with the tokens `file` gives an RGB value set from it:
    /// a colour without one (an ANSI colour, a terminal's) is the built-in palette's.
    pub fn new(theme: egui::Theme, file: Option<&ThemeFile>) -> Self {
        let (builtin, base) = match theme {
            egui::Theme::Dark => (BuiltinTheme::Dark, Visuals::dark()),
            egui::Theme::Light => (BuiltinTheme::Light, Visuals::light()),
        };
        let mut palette = builtin.palette();
        if builtin == BuiltinTheme::Light {
            // The terminal's warm track reads as beige beside the window's cool greys.
            palette[Token::BarEmpty].fg.rgb = Some((0xe3, 0xe7, 0xee));
        }
        if let Some(file) = file {
            let theirs = file.palette();
            for t in Token::ALL {
                let (mine, theirs) = (&mut palette[t], theirs[t]);
                if theirs.fg.rgb.is_some() {
                    mine.fg = theirs.fg;
                }
                if theirs.bg.rgb.is_some() {
                    mine.bg = theirs.bg;
                }
                if theirs.fg.rgb.is_some() || theirs.bg.rgb.is_some() {
                    mine.modifiers = theirs.modifiers;
                }
            }
        }
        Colors {
            palette,
            text: base.text_color(),
            chrome: Chrome::of(theme),
        }
    }

    /// The foreground of `t`.
    pub fn fg(&self, t: Token) -> Color32 {
        self.palette[t].fg.rgb.map_or(self.text, color)
    }

    /// The background of `t`, when it has one.
    pub fn bg(&self, t: Token) -> Option<Color32> {
        self.palette[t].bg.rgb.map(color)
    }

    /// `text` in the colour and modifiers of `t`. Dim text is drawn weaker; blinking, hidden and
    /// reversed text are a terminal's and left out.
    pub fn text(&self, text: impl Into<String>, t: Token) -> RichText {
        let m = self.palette[t].modifiers;
        let mut r = RichText::new(text).color(self.fg(t));
        if m.contains(Modifiers::BOLD) {
            r = r.strong();
        }
        if m.contains(Modifiers::DIM) {
            r = r.weak();
        }
        if m.contains(Modifiers::ITALIC) {
            r = r.italics();
        }
        if m.contains(Modifiers::UNDERLINED) {
            r = r.underline();
        }
        if m.contains(Modifiers::CROSSED_OUT) {
            r = r.strikethrough();
        }
        r
    }

    /// The accent: the main action's fill and the selected segment (the palette's `key`).
    pub fn accent(&self) -> Color32 {
        self.fg(Token::Key)
    }

    /// egui's `base` visuals in the palette's and the chrome's colours.
    pub fn visuals(&self, base: Visuals) -> Visuals {
        let cursor_bg = self.bg(Token::Cursor).unwrap_or(self.chrome.hover);
        let mut v = style::visuals(
            base,
            &self.chrome,
            self.fg(Token::Fg),
            self.fg(Token::Border),
            self.fg(Token::BorderFocus),
            self.accent(),
            (self.fg(Token::Cursor), cursor_bg),
            self.fg(Token::Key),
        );
        v.warn_fg_color = self.fg(Token::StatusWarn);
        v.error_fg_color = self.fg(Token::StatusError);
        v
    }
}

/// Bundles the fonts and sets the text styles and spacing, once, for both of egui's themes.
pub fn install(ctx: &egui::Context) {
    style::fonts(ctx);
    ctx.all_styles_mut(|s| {
        style::text_styles(s);
        style::spacing(s);
    });
}

/// Gives both of egui's themes the palette's colours, with those of `file`; egui shows the one
/// the system asks for, or the one chosen.
pub fn apply(ctx: &egui::Context, file: Option<&ThemeFile>) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let base = match theme {
            egui::Theme::Dark => Visuals::dark(),
            egui::Theme::Light => Visuals::light(),
        };
        ctx.set_visuals_of(theme, Colors::new(theme, file).visuals(base));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_take_the_palette_rgb_and_fall_back_to_egui_text() {
        let dark = Colors::new(egui::Theme::Dark, None);
        let light = Colors::new(egui::Theme::Light, None);
        assert_eq!(dark.fg(Token::Dir), Color32::from_rgb(0x8f, 0xbc, 0xf0));
        assert_eq!(light.fg(Token::Dir), Color32::from_rgb(0x1d, 0x5e, 0xa8));
        // `bg` is the terminal's own background: no colour of its own.
        assert_eq!(dark.bg(Token::Bg), None);
        assert_eq!(
            dark.bg(Token::Cursor),
            Some(Color32::from_rgb(0x4c, 0x56, 0x6a))
        );
        let v = dark.visuals(Visuals::dark());
        assert_eq!(v.override_text_color, Some(dark.fg(Token::Fg)));
        assert_eq!(v.selection.bg_fill, Color32::from_rgb(0x4c, 0x56, 0x6a));
        assert_eq!(v.panel_fill, Chrome::DARK.bg);
    }

    #[test]
    fn a_theme_file_sets_its_rgb_tokens_and_leaves_its_ansi_ones_to_the_built_in() {
        let file = ThemeFile::parse("base = \"ansi\"\n[tokens]\ndir = \"#123456\"\nfg = \"red\"\n")
            .expect("parses");
        let dark = Colors::new(egui::Theme::Dark, None);
        let mine = Colors::new(egui::Theme::Dark, Some(&file));
        assert_eq!(mine.fg(Token::Dir), Color32::from_rgb(0x12, 0x34, 0x56));
        // A named ANSI colour, and the ANSI base's tokens, are the built-in dark theme's.
        assert_eq!(mine.fg(Token::Fg), dark.fg(Token::Fg));
        assert_eq!(mine.fg(Token::Mark), dark.fg(Token::Mark));
    }
}
