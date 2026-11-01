//! Semantic styles. Widgets use tokens (`fg.dim`, `dir`, `status.warn`, ...), never literal
//! colours. A theme's palette (`dfdu_session::palette`) gives each token a colour per colour
//! level and modifiers; it is resolved once for the terminal's level into plain `Style`s. Every
//! colour cue also has a glyph or text, so `mono` (`NO_COLOR`) keeps the modifiers and drops
//! every colour.

#[cfg(test)]
mod builtin;
mod color;
mod detect;
mod file;

use dfdu_session::palette::{ColorSpec, Modifiers, Palette, Spec};
use ratatui::style::{Modifier, Style};

pub use crate::format::Bars;
pub use color::{ColorLevel, color_rgb, contrast, lightness, rgb_color};
pub use detect::{
    choose_theme, detect_color_level, parse_color_flag, query_background, should_query_background,
};
pub use dfdu_session::palette::{
    BuiltinTheme, Category, Rgb, SafetyStyle, ThemeError, ThemeFile, Token,
};

/// `m` as ratatui's modifiers.
fn modifier(m: Modifiers) -> Modifier {
    const PAIRS: [(Modifiers, Modifier); 9] = [
        (Modifiers::BOLD, Modifier::BOLD),
        (Modifiers::DIM, Modifier::DIM),
        (Modifiers::ITALIC, Modifier::ITALIC),
        (Modifiers::UNDERLINED, Modifier::UNDERLINED),
        (Modifiers::SLOW_BLINK, Modifier::SLOW_BLINK),
        (Modifiers::RAPID_BLINK, Modifier::RAPID_BLINK),
        (Modifiers::REVERSED, Modifier::REVERSED),
        (Modifiers::HIDDEN, Modifier::HIDDEN),
        (Modifiers::CROSSED_OUT, Modifier::CROSSED_OUT),
    ];
    PAIRS
        .into_iter()
        .filter(|&(ours, _)| m.contains(ours))
        .fold(Modifier::empty(), |acc, (_, r)| acc | r)
}

/// `spec` resolved for `level`.
fn spec_style(spec: &Spec, level: ColorLevel) -> Style {
    let mut s = Style::new().add_modifier(modifier(spec.modifiers));
    if let Some(c) = color::resolve(&spec.fg, level) {
        s = s.fg(c);
    }
    if let Some(c) = color::resolve(&spec.bg, level) {
        s = s.bg(c);
    }
    s
}

/// A theme resolved for one colour level. Each field is the style of the token of the same
/// name, with `_` for `.` (`fg_dim` is `fg.dim`); <https://dfdu.app/docs/themes/> says what
/// each is used for.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub fg: Style,
    pub fg_dim: Style,
    pub bg: Style,
    pub border: Style,
    pub border_focus: Style,
    pub header: Style,
    pub title: Style,
    pub cursor: Style,
    pub mark: Style,
    pub dir: Style,
    pub key: Style,
    pub bar_fill: Style,
    pub bar_empty: Style,
    pub size_big: Style,
    pub scan_active: Style,
    pub scan_done: Style,
    pub status_error: Style,
    pub status_warn: Style,
    pub status_ok: Style,
    pub r#match: Style,
    /// Indexed by `Category as usize`.
    pub cat: [Style; 8],
    /// Indexed by `SafetyStyle as usize`.
    pub safety: [Style; 3],
    /// How bars and the treemap's frames are drawn.
    pub bars: Bars,
    name: String,
    level: ColorLevel,
    background: Option<Rgb>,
}

impl Theme {
    /// The 16-colour theme (`ansi`), readable on light and dark backgrounds.
    #[must_use]
    pub fn ansi16() -> Self {
        Self::builtin(BuiltinTheme::Ansi, ColorLevel::Ansi16, None)
    }

    /// The theme without colours (`mono`).
    #[must_use]
    pub fn no_color() -> Self {
        Self::builtin(BuiltinTheme::Mono, ColorLevel::None, None)
    }

    /// The names [`Theme::named`] accepts.
    #[must_use]
    pub fn builtin_names() -> &'static [&'static str] {
        &BuiltinTheme::NAMES
    }

    /// Built-in theme `name` resolved for `level`, on a terminal whose background is
    /// `background` when known. `mono` always resolves without colours.
    #[must_use]
    pub fn named(name: &str, level: ColorLevel, background: Option<Rgb>) -> Option<Theme> {
        BuiltinTheme::parse(name).map(|b| Self::builtin(b, level, background))
    }

    /// Built-in theme `b` resolved for `level`, on a terminal whose background is `background`
    /// when known. `mono` always resolves without colours.
    #[must_use]
    pub fn builtin(b: BuiltinTheme, level: ColorLevel, background: Option<Rgb>) -> Self {
        let level = if b == BuiltinTheme::Mono {
            ColorLevel::None
        } else {
            level
        };
        Self::resolve(b.name().to_owned(), &b.palette(), level, background)
    }

    /// Resolves every token of `palette` for `level`. Without colours, each token that has a
    /// colour also gets the `mono` theme's modifiers, so the cursor, marks and matches stay
    /// visible.
    fn resolve(
        name: String,
        palette: &Palette,
        level: ColorLevel,
        background: Option<Rgb>,
    ) -> Self {
        let mono = (level == ColorLevel::None).then(|| BuiltinTheme::Mono.palette());
        let style = |t: Token| {
            let mut spec = palette[t];
            if let Some(mono) = &mono
                && (spec.fg != ColorSpec::NONE || spec.bg != ColorSpec::NONE)
            {
                spec.modifiers |= mono[t].modifiers;
            }
            spec_style(&spec, level)
        };
        Theme {
            fg: style(Token::Fg),
            fg_dim: style(Token::FgDim),
            bg: style(Token::Bg),
            border: style(Token::Border),
            border_focus: style(Token::BorderFocus),
            header: style(Token::Header),
            title: style(Token::Title),
            cursor: style(Token::Cursor),
            mark: style(Token::Mark),
            dir: style(Token::Dir),
            key: style(Token::Key),
            bar_fill: style(Token::BarFill),
            bar_empty: style(Token::BarEmpty),
            size_big: style(Token::SizeBig),
            scan_active: style(Token::ScanActive),
            scan_done: style(Token::ScanDone),
            status_error: style(Token::StatusError),
            status_warn: style(Token::StatusWarn),
            status_ok: style(Token::StatusOk),
            r#match: style(Token::Match),
            cat: Category::ALL.map(|c| style(c.token())),
            safety: SafetyStyle::ALL.map(|s| style(s.token())),
            bars: Bars::default(),
            name,
            level,
            background,
        }
    }

    /// The style of `token`.
    #[cfg(test)]
    #[must_use]
    pub fn style(&self, token: Token) -> Style {
        match token {
            Token::Fg => self.fg,
            Token::FgDim => self.fg_dim,
            Token::Bg => self.bg,
            Token::Border => self.border,
            Token::BorderFocus => self.border_focus,
            Token::Header => self.header,
            Token::Title => self.title,
            Token::Cursor => self.cursor,
            Token::Mark => self.mark,
            Token::Dir => self.dir,
            Token::Key => self.key,
            Token::BarFill => self.bar_fill,
            Token::BarEmpty => self.bar_empty,
            Token::SizeBig => self.size_big,
            Token::ScanActive => self.scan_active,
            Token::ScanDone => self.scan_done,
            Token::StatusError => self.status_error,
            Token::StatusWarn => self.status_warn,
            Token::StatusOk => self.status_ok,
            Token::Match => self.r#match,
            Token::CatCache => self.cat[Category::Cache as usize],
            Token::CatBuild => self.cat[Category::Build as usize],
            Token::CatVm => self.cat[Category::Vm as usize],
            Token::CatToolchain => self.cat[Category::Toolchain as usize],
            Token::CatMedia => self.cat[Category::Media as usize],
            Token::CatDownload => self.cat[Category::Download as usize],
            Token::CatContainer => self.cat[Category::Container as usize],
            Token::CatOther => self.cat[Category::Other as usize],
            Token::SafetySafe => self.safety[SafetyStyle::Safe as usize],
            Token::SafetyReview => self.safety[SafetyStyle::Review as usize],
            Token::SafetyKeep => self.safety[SafetyStyle::Keep as usize],
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The colour level the styles were resolved for; RGB effects need `TrueColor`.
    #[must_use]
    pub fn level(&self) -> ColorLevel {
        self.level
    }

    /// The terminal background the theme was resolved for, when it was queried.
    #[must_use]
    pub fn background(&self) -> Option<Rgb> {
        self.background
    }
}

#[cfg(test)]
mod tests;
