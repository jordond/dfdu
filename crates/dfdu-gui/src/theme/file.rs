//! The theme a name chooses, as `dfdu --theme NAME` reads it ([`named`]): `auto`, a built-in
//! theme, or the theme file `NAME.toml` in dfdu's theme directory. The terminal's `ansi`, `mono`
//! and `high-contrast` have no colours of their own for a window: they follow the system's dark
//! or light setting, as `auto` does. A theme file draws on the dark or light theme of its base
//! ([`preference`]).

use std::ffi::OsStr;
use std::path::Path;

use dfdu_session::palette::{BuiltinTheme, ThemeFile};

use crate::settings::ThemeChoice;

/// The theme `name` chooses, with the theme file it names read; `themes` is the directory of
/// theme files (`None` when the platform has no configuration directory).
///
/// # Errors
///
/// As `dfdu --theme` says them: `name` is no built-in theme and no theme file's name, there is
/// no such file, or it cannot be read or is not a theme file.
pub fn named(
    name: &str,
    themes: Option<&Path>,
) -> Result<(ThemeChoice, Option<ThemeFile>), String> {
    if name == "auto" {
        return Ok((ThemeChoice::System, None));
    }
    if let Some(b) = BuiltinTheme::parse(name) {
        let choice = match b {
            BuiltinTheme::Dark => ThemeChoice::Dark,
            BuiltinTheme::Light => ThemeChoice::Light,
            BuiltinTheme::Ansi | BuiltinTheme::Mono | BuiltinTheme::HighContrast => {
                ThemeChoice::System
            }
        };
        return Ok((choice, None));
    }
    let builtin = format!("auto, {}", BuiltinTheme::NAMES.join(", "));
    if Path::new(name).file_name() != Some(OsStr::new(name)) {
        return Err(format!(
            "not a built-in theme ({builtin}) or the name of a theme file"
        ));
    }
    let Some(themes) = themes else {
        return Err(format!(
            "not a built-in theme ({builtin}), and there is no configuration directory for theme \
             files"
        ));
    };
    let path = themes.join(format!("{name}.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "not a built-in theme ({builtin}), and there is no file {}",
                path.display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut file = ThemeFile::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    file.name.get_or_insert_with(|| name.to_owned());
    Ok((ThemeChoice::File(name.to_owned()), Some(file)))
}

/// The names of the theme files in `themes` that [`named`] reads as theme files, sorted:
/// `NAME.toml` for each `NAME` that is not a built-in theme's. None when the directory is not
/// there or cannot be read.
pub fn listed(themes: Option<&Path>) -> Vec<String> {
    let Some(entries) = themes.and_then(|d| std::fs::read_dir(d).ok()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".toml")?;
            let file = e.file_type().ok()?.is_file() || e.path().is_file();
            let builtin = stem == "auto" || BuiltinTheme::parse(stem).is_some();
            (file && !stem.is_empty() && !builtin).then(|| stem.to_owned())
        })
        .collect();
    names.sort();
    names
}

/// The theme file `choice` names, read from `themes`, when it names one. When it cannot be
/// read (it was removed, or no longer reads), standard error says why and the choice goes back
/// to the system's.
pub fn saved(choice: &mut ThemeChoice, themes: Option<&Path>) -> Option<ThemeFile> {
    let ThemeChoice::File(name) = choice else {
        return None;
    };
    match named(name, themes) {
        Ok((_, file)) => file,
        Err(e) => {
            eprintln!("dfdu-gui: theme {name}: {e}; using the system's");
            *choice = ThemeChoice::System;
            None
        }
    }
}

/// egui's preference for `choice`; a theme file's is its base's, the system's for a base
/// without colours of its own.
pub fn preference(choice: &ThemeChoice, file: Option<&ThemeFile>) -> egui::ThemePreference {
    match choice {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::File(_) => match file.map(ThemeFile::base) {
            Some(BuiltinTheme::Dark) => egui::ThemePreference::Dark,
            Some(BuiltinTheme::Light) => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        },
    }
}

/// The theme of the window's title bar for egui's preference `p`: the system's, unless a
/// dark or a light theme is chosen.
pub fn title_bar(p: egui::ThemePreference) -> egui::SystemTheme {
    match p {
        egui::ThemePreference::System => egui::SystemTheme::SystemDefault,
        egui::ThemePreference::Dark => egui::SystemTheme::Dark,
        egui::ThemePreference::Light => egui::SystemTheme::Light,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_name_is_a_built_in_theme_or_a_theme_file_as_dfdu_reads_it() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let themes = tmp.path();
        let write = |name: &str, text: &str| fs::write(themes.join(name), text).expect("write");
        write(
            "nord.toml",
            "base = \"light\"\n[tokens]\nfg = \"#2e3440\"\n",
        );
        write("broken.toml", "base = \"sepia\"\n");

        assert_eq!(named("auto", None), Ok((ThemeChoice::System, None)));
        assert_eq!(named("dark", None), Ok((ThemeChoice::Dark, None)));
        // A terminal's themes follow the system's setting.
        for terminal in ["ansi", "mono", "high-contrast"] {
            assert_eq!(named(terminal, None), Ok((ThemeChoice::System, None)));
        }
        let (choice, file) = named("nord", Some(themes)).expect("nord");
        assert_eq!(choice, ThemeChoice::File("nord".into()));
        let file = file.expect("the file");
        assert_eq!(file.theme_name(), "nord");
        assert_eq!(
            preference(&choice, Some(&file)),
            egui::ThemePreference::Light
        );
        assert_eq!(
            title_bar(preference(&choice, Some(&file))),
            egui::SystemTheme::Light
        );
        let system = preference(&ThemeChoice::System, None);
        assert_eq!(title_bar(system), egui::SystemTheme::SystemDefault);

        let err = |name: &str| named(name, Some(themes)).expect_err(name);
        assert!(err("missing").contains("missing.toml"));
        assert!(err("broken").contains("sepia"));
        assert!(err("../nord").contains("not a built-in theme"));
        assert!(named("nord", None).is_err());

        // A choice kept from the last run reads its file again, or goes back to the system's.
        let mut kept = ThemeChoice::File("nord".into());
        assert!(saved(&mut kept, Some(themes)).is_some());
        let mut gone = ThemeChoice::File("gone".into());
        assert!(saved(&mut gone, Some(themes)).is_none());
        assert_eq!(gone, ThemeChoice::System);
    }

    #[test]
    fn the_theme_files_listed_are_those_a_name_reads() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let themes = tmp.path();
        for name in ["nord.toml", "dracula.toml", "dark.toml", "notes.txt"] {
            fs::write(themes.join(name), "base = \"dark\"\n").expect("write");
        }
        fs::create_dir(themes.join("dir.toml")).expect("mkdir");
        // A built-in theme's name chooses the built-in theme, not the file.
        assert_eq!(listed(Some(themes)), ["dracula", "nord"]);
        assert!(listed(Some(&themes.join("missing"))).is_empty());
        assert!(listed(None).is_empty());
    }
}
