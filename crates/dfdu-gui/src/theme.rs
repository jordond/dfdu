//! The theme's tokens as egui colours and text styles. The built-in dark and light palettes of
//! `dfdu_session::palette` follow the system's setting; a token without an RGB value (the
//! terminal's own colours) takes egui's colour for text or for the background.

use dfdu_session::palette::{BuiltinTheme, Modifiers, Palette, Rgb, Token};
use egui::{Color32, RichText, Visuals};

/// The palette of one of egui's themes, resolved to colours.
#[derive(Clone, Debug)]
pub struct Colors {
    palette: Palette,
    /// egui's text colour, for tokens without a foreground.
    text: Color32,
}

fn color((r, g, b): Rgb) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Colors {
    /// The built-in palette for `theme`.
    pub fn of(theme: egui::Theme) -> Self {
        let (builtin, base) = match theme {
            egui::Theme::Dark => (BuiltinTheme::Dark, Visuals::dark()),
            egui::Theme::Light => (BuiltinTheme::Light, Visuals::light()),
        };
        Colors {
            palette: builtin.palette(),
            text: base.text_color(),
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

    /// egui's `base` visuals with the palette's text, selection and accent colours.
    pub fn visuals(&self, mut base: Visuals) -> Visuals {
        base.override_text_color = Some(self.fg(Token::Fg));
        if let Some(bg) = self.bg(Token::Cursor) {
            base.selection.bg_fill = bg;
        }
        base.selection.stroke.color = self.fg(Token::Cursor);
        base.hyperlink_color = self.fg(Token::Key);
        base.warn_fg_color = self.fg(Token::StatusWarn);
        base.error_fg_color = self.fg(Token::StatusError);
        base
    }
}

/// Gives both of egui's themes the palette's colours; egui shows the one the system asks for.
pub fn apply(ctx: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let base = match theme {
            egui::Theme::Dark => Visuals::dark(),
            egui::Theme::Light => Visuals::light(),
        };
        ctx.set_visuals_of(theme, Colors::of(theme).visuals(base));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_take_the_palette_rgb_and_fall_back_to_egui_text() {
        let dark = Colors::of(egui::Theme::Dark);
        let light = Colors::of(egui::Theme::Light);
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
    }
}
