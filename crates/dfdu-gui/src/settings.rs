//! The settings kept between runs in eframe's storage, beside the window's size and position
//! (which eframe keeps itself): the theme chosen, and how the last browser showed its list
//! (tree mode, the details panel). They are one line of `key=value` pairs under [`KEY`]; a
//! value that does not read is left at its default.
//!
//! The storage is `gui.ron` in dfdu's state directory (`DFDU_STATE_DIR` when set), beside the
//! scan history ([`path`]).

use std::path::PathBuf;

/// The key of the settings in eframe's storage.
pub const KEY: &str = "dfdu-gui";

/// Which theme the window draws with.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ThemeChoice {
    /// The system's dark or light setting.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light];

    /// The name stored, and shown in the menu in capitals.
    pub fn name(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// egui's preference for it.
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        }
    }
}

/// The settings kept between runs.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
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
        for theme in ThemeChoice::ALL {
            for (tree, details) in [(false, false), (true, false), (false, true), (true, true)] {
                let p = Prefs {
                    theme,
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
