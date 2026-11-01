//! The theme's tokens and their colours, with no terminal or window types. Widgets use tokens
//! (`fg.dim`, `dir`, `status.warn`, ...), never literal colours. A [`Palette`] gives each token
//! a foreground and a background, with a value per colour level, and modifiers. It comes from a
//! built-in theme ([`BuiltinTheme::palette`]) or a theme file ([`ThemeFile::palette`]), and each
//! front end resolves it to its own styles: a terminal for its colour level, a window from the
//! RGB values. So one theme file colours both.

mod builtin;
mod color;
mod file;

use std::ops::{Index, IndexMut};

pub use builtin::BuiltinTheme;
pub use color::{AnsiColor, ColorSpec, Modifiers, Rgb};
pub use file::{BARS, ThemeError, ThemeFile};

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

/// What a directory's contents are, for category tags. A front end keeps a colour slot per
/// category, indexed by `Category as usize`.
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

/// The style of a cleanup candidate by how safe it is to delete, indexed by
/// `SafetyStyle as usize` like [`Category`]. Three buckets for colour, not the rules'
/// [`dfdu_tree::Safety`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SafetyStyle {
    Safe,
    Review,
    Keep,
}

impl SafetyStyle {
    pub const ALL: [SafetyStyle; 3] = [SafetyStyle::Safe, SafetyStyle::Review, SafetyStyle::Keep];

    pub const fn token(self) -> Token {
        match self {
            SafetyStyle::Safe => Token::SafetySafe,
            SafetyStyle::Review => Token::SafetyReview,
            SafetyStyle::Keep => Token::SafetyKeep,
        }
    }
}

/// A token's value: foreground and background with a value per colour level, and modifiers that
/// apply at every level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub fg: ColorSpec,
    pub bg: ColorSpec,
    pub modifiers: Modifiers,
}

impl Spec {
    pub const NONE: Spec = Spec {
        fg: ColorSpec::NONE,
        bg: ColorSpec::NONE,
        modifiers: Modifiers::empty(),
    };

    #[must_use]
    pub const fn fg(fg: ColorSpec) -> Self {
        Spec { fg, ..Self::NONE }
    }

    #[must_use]
    pub const fn bg(self, bg: ColorSpec) -> Self {
        Spec { bg, ..self }
    }

    #[must_use]
    pub const fn add(self, m: Modifiers) -> Self {
        Spec {
            modifiers: self.modifiers.union(m),
            ..self
        }
    }
}

/// Every token's value. A front end asks for one by token: `palette[Token::Dir]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette([Spec; Token::COUNT]);

impl Palette {
    /// No colours and no modifiers for any token.
    pub const NONE: Palette = Palette([Spec::NONE; Token::COUNT]);

    /// The palette that gives each token `spec(token)`.
    #[must_use]
    pub fn from_fn(spec: impl FnMut(Token) -> Spec) -> Self {
        Palette(Token::ALL.map(spec))
    }
}

impl Index<Token> for Palette {
    type Output = Spec;

    fn index(&self, t: Token) -> &Spec {
        &self.0[t as usize]
    }
}

impl IndexMut<Token> for Palette {
    fn index_mut(&mut self, t: Token) -> &mut Spec {
        &mut self.0[t as usize]
    }
}

#[cfg(test)]
mod tests;
