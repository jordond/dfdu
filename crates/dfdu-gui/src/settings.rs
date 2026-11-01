//! The settings kept between runs in eframe's storage, beside the window's size and position
//! (which eframe keeps itself): the theme chosen (a built-in one, or a theme file by its name),
//! and how the last browser showed its list (tree mode, the details panel). They are one line
//! of `key=value` pairs under [`KEY`]; a value that does not read is left at its default.
//!
//! The storage is `gui.ron` in dfdu's state directory (`DFDU_STATE_DIR` when set), beside the
//! scan history ([`path`]).

use std::path::PathBuf;

/// The key of the settings in eframe's storage.
pub const KEY: &str = "dfdu-gui";

/// Which theme the window draws with.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ThemeChoice {
    /// The system's dark or light setting.
    #[default]
    System,
    Dark,
    Light,
    /// The theme file `NAME.toml` in dfdu's theme directory, by `NAME`.
    File(String),
}

impl ThemeChoice {
    /// The built-in choices, in the order the menu lists them.
    pub const BUILTIN: [ThemeChoice; 3] =
        [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light];

    /// The name stored: a built-in's, or `file:NAME` for a theme file.
    pub fn name(&self) -> String {
        match self {
            ThemeChoice::System => "system".into(),
            ThemeChoice::Dark => "dark".into(),
            ThemeChoice::Light => "light".into(),
            ThemeChoice::File(name) => format!("file:{name}"),
        }
    }

    /// What the menu and the palette call it: a theme file by its name.
    pub fn label(&self) -> &str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
            ThemeChoice::File(name) => name,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name.strip_prefix("file:") {
            Some(file) if !file.is_empty() => Some(ThemeChoice::File(file.to_owned())),
            Some(_) => None,
            None => Self::BUILTIN.into_iter().find(|t| t.name() == name),
        }
    }
}

/// The settings kept between runs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Prefs {
    pub theme: ThemeChoice,
    /// The list was in tree mode.
    pub tree: bool,
    /// The details panel was open.
    pub details: bool,
}

impl Prefs {
    /// `theme=dark tree=1 details=0`.
    pub fn encode(&self) -> String {
        format!(
            "theme={} tree={} details={}",
            self.theme.name(),
            u8::from(self.tree),
            u8::from(self.details)
        )
    }

    /// The settings `text` stores; those missing or that do not read are the defaults.
    pub fn decode(text: &str) -> Self {
        let mut p = Prefs::default();
        for pair in text.split_whitespace() {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let flag = match value {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            };
            match key {
                "theme" => p.theme = ThemeChoice::from_name(value).unwrap_or(p.theme),
                "tree" => p.tree = flag.unwrap_or(p.tree),
                "details" => p.details = flag.unwrap_or(p.details),
                _ => {}
            }
        }
        p
    }

    /// The settings in `storage`, the defaults without one.
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|s| s.get_string(KEY))
            .map_or_else(Prefs::default, |t| Prefs::decode(&t))
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string(KEY, self.encode());
    }
}

/// Where eframe keeps the settings: `gui.ron` in dfdu's state directory, `None` (eframe's own
/// place) when the platform has none.
pub fn path() -> Option<PathBuf> {
    Some(dfdu_sys::dirs::state_dir()?.join("gui.ron"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_back_as_written() {
        let themes = ThemeChoice::BUILTIN
            .into_iter()
            .chain([ThemeChoice::File("nord".into())]);
        for theme in themes {
            for (tree, details) in [(false, false), (true, false), (false, true), (true, true)] {
                let p = Prefs {
                    theme: theme.clone(),
                    tree,
                    details,
                };
                assert_eq!(Prefs::decode(&p.encode()), p);
            }
        }
        assert_eq!(
            Prefs {
                theme: ThemeChoice::Dark,
                tree: true,
                details: false,
            }
            .encode(),
            "theme=dark tree=1 details=0"
        );
    }

    #[test]
    fn what_does_not_read_is_left_at_its_default() {
        assert_eq!(Prefs::decode(""), Prefs::default());
        assert_eq!(Prefs::decode("theme=file:").theme, ThemeChoice::System);
        assert_eq!(
            Prefs::decode("theme=file:nord").theme,
            ThemeChoice::File("nord".into())
        );
        let p = Prefs::decode("theme=sepia tree=yes details=1 font=12 junk");
        assert_eq!(
            p,
            Prefs {
                theme: ThemeChoice::System,
                tree: false,
                details: true,
            }
        );
    }
}
