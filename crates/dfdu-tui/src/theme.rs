//! Semantic styles. Widgets use tokens (`fg.dim`, `dir`, `status.warn`, ...), never literal
//! colours. A theme gives each token a colour per colour level and modifiers; it is resolved once
//! for the terminal's level into plain `Style`s. Every colour cue also has a glyph or text, so
//! `mono` (`NO_COLOR`) keeps the modifiers and drops every colour.

mod builtin;
mod color;
mod detect;
mod file;
mod format;

use ratatui::style::{Modifier, Style};

pub use color::{ColorLevel, ColorSpec, Rgb};
pub use detect::{
    choose_theme, detect_color_level, parse_color_flag, query_background, should_query_background,
};
pub use dfdu_engine::human;
pub use file::{ThemeError, ThemeFile};
pub use format::{bar, count, percent, ratio, spinner};

/// A themable style. [`Token::name`] is its key in a theme file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    Fg,
    FgDim,
    Bg,
    Border,
    BorderFocus,
    Header,
    Title,
    Cursor,
    Mark,
    Dir,
    Key,
    BarFill,
    BarEmpty,
    SizeBig,
    ScanActive,
    ScanDone,
    StatusError,
    StatusWarn,
    StatusOk,
    Match,
    CatCache,
    CatBuild,
    CatVm,
    CatToolchain,
    CatMedia,
    CatDownload,
    CatContainer,
    CatOther,
    SafetySafe,
    SafetyReview,
    SafetyKeep,
}

impl Token {
    pub const COUNT: usize = 31;
    pub const ALL: [Token; Self::COUNT] = [
        Token::Fg,
        Token::FgDim,
        Token::Bg,
        Token::Border,
        Token::BorderFocus,
        Token::Header,
        Token::Title,
        Token::Cursor,
        Token::Mark,
        Token::Dir,
        Token::Key,
        Token::BarFill,
        Token::BarEmpty,
        Token::SizeBig,
        Token::ScanActive,
        Token::ScanDone,
        Token::StatusError,
        Token::StatusWarn,
        Token::StatusOk,
        Token::Match,
        Token::CatCache,
        Token::CatBuild,
        Token::CatVm,
        Token::CatToolchain,
        Token::CatMedia,
        Token::CatDownload,
        Token::CatContainer,
        Token::CatOther,
        Token::SafetySafe,
        Token::SafetyReview,
        Token::SafetyKeep,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Token::Fg => "fg",
            Token::FgDim => "fg.dim",
            Token::Bg => "bg",
            Token::Border => "border",
            Token::BorderFocus => "border.focus",
            Token::Header => "header",
            Token::Title => "title",
            Token::Cursor => "cursor",
            Token::Mark => "mark",
            Token::Dir => "dir",
            Token::Key => "key",
            Token::BarFill => "bar.fill",
            Token::BarEmpty => "bar.empty",
            Token::SizeBig => "size.big",
            Token::ScanActive => "scan.active",
            Token::ScanDone => "scan.done",
            Token::StatusError => "status.error",
            Token::StatusWarn => "status.warn",
            Token::StatusOk => "status.ok",
            Token::Match => "match",
            Token::CatCache => "cat.cache",
            Token::CatBuild => "cat.build",
            Token::CatVm => "cat.vm",
            Token::CatToolchain => "cat.toolchain",
            Token::CatMedia => "cat.media",
            Token::CatDownload => "cat.download",
            Token::CatContainer => "cat.container",
            Token::CatOther => "cat.other",
            Token::SafetySafe => "safety.safe",
            Token::SafetyReview => "safety.review",
            Token::SafetyKeep => "safety.keep",
        }
    }

    pub fn from_name(name: &str) -> Option<Token> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }
}

/// What a directory's contents are, for category tags; indexes [`Theme::cat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Cache,
    Build,
    Vm,
    Toolchain,
    Media,
    Download,
    Container,
    Other,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::Cache,
        Category::Build,
        Category::Vm,
        Category::Toolchain,
        Category::Media,
        Category::Download,
        Category::Container,
        Category::Other,
    ];

    pub const fn token(self) -> Token {
        match self {
            Category::Cache => Token::CatCache,
            Category::Build => Token::CatBuild,
            Category::Vm => Token::CatVm,
            Category::Toolchain => Token::CatToolchain,
            Category::Media => Token::CatMedia,
            Category::Download => Token::CatDownload,
            Category::Container => Token::CatContainer,
            Category::Other => Token::CatOther,
        }
    }
}

/// How safe a cleanup candidate is to delete; indexes [`Theme::safety`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Safety {
    Safe,
    Review,
    Keep,
}

impl Safety {
    pub const ALL: [Safety; 3] = [Safety::Safe, Safety::Review, Safety::Keep];

    pub const fn token(self) -> Token {
        match self {
            Safety::Safe => Token::SafetySafe,
            Safety::Review => Token::SafetyReview,
            Safety::Keep => Token::SafetyKeep,
        }
    }
}

/// A token's unresolved value: foreground and background per level, and modifiers that apply at
/// every level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub fg: ColorSpec,
    pub bg: ColorSpec,
    pub modifiers: Modifier,
}

impl Spec {
    pub const NONE: Spec = Spec {
        fg: ColorSpec::NONE,
        bg: ColorSpec::NONE,
        modifiers: Modifier::empty(),
    };

    pub const fn fg(fg: ColorSpec) -> Self {
        Spec { fg, ..Self::NONE }
    }

    pub const fn bg(self, bg: ColorSpec) -> Self {
        Spec { bg, ..self }
    }

    pub const fn add(self, m: Modifier) -> Self {
        Spec {
            modifiers: self.modifiers.union(m),
            ..self
        }
    }

    fn resolve(&self, level: ColorLevel) -> Style {
        let mut s = Style::new().add_modifier(self.modifiers);
        if let Some(c) = self.fg.resolve(level) {
            s = s.fg(c);
        }
        if let Some(c) = self.bg.resolve(level) {
            s = s.bg(c);
        }
        s
    }
}

/// Every token's value, indexed by `Token as usize`.
pub type Palette = [Spec; Token::COUNT];

/// A theme resolved for one colour level. Each field is the style of the token of the same
/// name, with `_` for `.` (`fg_dim` is `fg.dim`); docs/themes.md says what each is used for.
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
    /// Indexed by `Safety as usize`.
    pub safety: [Style; 3],
    name: String,
    level: ColorLevel,
    background: Option<Rgb>,
}

impl Theme {
    /// The 16-colour theme (`ansi`), readable on light and dark backgrounds.
    #[must_use]
    pub fn ansi16() -> Self {
        Self::builtin("ansi", ColorLevel::Ansi16, None)
    }

    /// The theme without colours (`mono`).
    #[must_use]
    pub fn no_color() -> Self {
        Self::builtin("mono", ColorLevel::None, None)
    }

    /// The names [`Theme::named`] accepts.
    #[must_use]
    pub fn builtin_names() -> &'static [&'static str] {
        &builtin::NAMES
    }

    /// Built-in theme `name` resolved for `level`, on a terminal whose background is
    /// `background` when known. `mono` always resolves without colours.
    #[must_use]
    pub fn named(name: &str, level: ColorLevel, background: Option<Rgb>) -> Option<Theme> {
        let palette = builtin::palette(name)?;
        let level = if name == "mono" {
            ColorLevel::None
        } else {
            level
        };
        Some(Self::resolve(name.to_owned(), &palette, level, background))
    }

    /// Built-in theme `name`, which must be one of [`builtin::NAMES`].
    #[expect(
        clippy::expect_used,
        reason = "only called with names from builtin::NAMES, which all have a palette"
    )]
    fn builtin(name: &str, level: ColorLevel, background: Option<Rgb>) -> Self {
        Self::named(name, level, background).expect("built-in theme")
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
        #[expect(
            clippy::expect_used,
            reason = "`mono` is one of builtin::NAMES, which all have a palette"
        )]
        let mono = (level == ColorLevel::None).then(|| builtin::palette("mono").expect("mono"));
        let style = |t: Token| {
            let mut spec = palette[t as usize];
            if let Some(mono) = &mono
                && (spec.fg != ColorSpec::NONE || spec.bg != ColorSpec::NONE)
            {
                spec.modifiers |= mono[t as usize].modifiers;
            }
            spec.resolve(level)
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
            safety: Safety::ALL.map(|s| style(s.token())),
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
            Token::SafetySafe => self.safety[Safety::Safe as usize],
            Token::SafetyReview => self.safety[Safety::Review as usize],
            Token::SafetyKeep => self.safety[Safety::Keep as usize],
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
mod tests {
    use super::*;

    #[test]
    fn token_names_round_trip() {
        for (i, t) in Token::ALL.into_iter().enumerate() {
            assert_eq!(t as usize, i, "{t:?} is in declaration order");
            assert_eq!(Token::from_name(t.name()), Some(t));
        }
        assert_eq!(Token::from_name("cursor.bg"), None);
        for c in Category::ALL {
            assert_eq!(
                c.token().name().strip_prefix("cat."),
                Some(format!("{c:?}").to_lowercase().as_str())
            );
        }
        for s in Safety::ALL {
            assert_eq!(
                s.token().name().strip_prefix("safety."),
                Some(format!("{s:?}").to_lowercase().as_str())
            );
        }
    }

    #[test]
    fn builtin_styles_keep_their_values() {
        use ratatui::style::Color;
        let t = Theme::ansi16();
        assert_eq!(t.title, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(t.fg_dim, Style::new().fg(Color::DarkGray));
        assert_eq!(t.bar_fill, Style::new().fg(Color::Cyan));
        assert_eq!(
            t.dir,
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
        );
        assert_eq!(t.cursor, Style::new().add_modifier(Modifier::REVERSED));
        assert_eq!(t.status_warn, Style::new().fg(Color::Yellow));
        assert_eq!(t.status_error, Style::new().fg(Color::Red));
        assert_eq!(t.key, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(
            (t.name(), t.level(), t.background()),
            ("ansi", ColorLevel::Ansi16, None)
        );

        let m = Theme::no_color();
        assert_eq!(m.title, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(m.fg_dim, Style::new().add_modifier(Modifier::DIM));
        assert_eq!(m.bar_fill, Style::new());
        assert_eq!(m.dir, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(m.cursor, Style::new().add_modifier(Modifier::REVERSED));
        assert_eq!(m.status_warn, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(m.status_error, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(m.key, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!((m.name(), m.level()), ("mono", ColorLevel::None));
    }

    #[test]
    fn named_themes_remember_how_they_were_resolved() {
        for &name in Theme::builtin_names() {
            let t = Theme::named(name, ColorLevel::TrueColor, Some((1, 2, 3))).expect(name);
            assert_eq!(t.name(), name);
            assert_eq!(t.background(), Some((1, 2, 3)));
            let want = if name == "mono" {
                ColorLevel::None
            } else {
                ColorLevel::TrueColor
            };
            assert_eq!(t.level(), want, "{name}");
        }
        assert!(Theme::named("solarized", ColorLevel::TrueColor, None).is_none());
    }

    #[test]
    fn every_token_resolves_into_its_own_field() {
        use ratatui::style::Color;
        let mut palette: Palette = [Spec::NONE; Token::COUNT];
        for t in Token::ALL {
            palette[t as usize] = Spec::fg(ColorSpec {
                ansi256: Some(t as u8),
                ..ColorSpec::NONE
            });
        }
        let theme = Theme::resolve("test".into(), &palette, ColorLevel::Ansi256, None);
        for t in Token::ALL {
            assert_eq!(theme.style(t).fg, Some(Color::Indexed(t as u8)), "{t:?}");
        }
        assert_eq!(theme.cursor, theme.style(Token::Cursor));
        assert_eq!(theme.r#match, theme.style(Token::Match));
        assert_eq!(
            theme.cat[Category::Media as usize],
            theme.style(Token::CatMedia)
        );
        assert_eq!(
            theme.safety[Safety::Keep as usize],
            theme.style(Token::SafetyKeep)
        );
    }
}
