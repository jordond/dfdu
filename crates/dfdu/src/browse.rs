//! `dfdu [PATH]` on a terminal: the interactive browser.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dfdu_engine::EngineOptions;
use dfdu_tui::{Source, StartLocal, Theme, ThemeChoice, ThemeFile, UiSettings};

use crate::launch::Local;
use crate::transfer::read_export;
use crate::{Cli, remote};

/// `dfdu [PATH]` on a terminal: the browser, on PATH, a disk from the picker, a scan read from a
/// file, or a directory on another host.
pub fn run(cli: Cli) -> Result<ExitCode, ExitCode> {
    let theme = match theme_choice(&cli.theme, dfdu_tui::theme_dir().as_deref()) {
        Ok(theme) => theme,
        Err(e) => {
            eprintln!("dfdu: theme {}: {e}", cli.theme);
            return Err(ExitCode::from(2));
        }
    };
    let engine = EngineOptions {
        read_only: cli.read_only,
        ..EngineOptions::from_env()
    };
    let path = cli.path.unwrap_or_else(|| PathBuf::from("."));
    let source = if let Some(f) = &cli.import {
        Source::Imported(read_export(f)?.tree)
    } else if let Some(host) = &cli.remote {
        // Connected before the browser takes the terminal, so ssh can ask for a password.
        // Deletion runs on the agent, journalled on its host.
        let rules = cli.rules.remote();
        let job = cli.mounts.job();
        let (engine, job, root) =
            remote::engine(host, engine.clone(), job, &path, rules, cli.remote_agent)?;
        Source::Remote {
            engine,
            path: root,
            job,
        }
    } else {
        let resumed = (cli.resume_from.as_deref().map(read_export))
            .transpose()?
            .map(|im| (im.tree, im.journal));
        let root = resumed
            .as_ref()
            .and_then(|(t, _)| t.roots().next().map(|r| t.path_buf(r)));
        // Checked before the browser takes the terminal. With --disks the directory is not
        // known yet, and --mft is not used.
        let local = Local {
            mft: !cli.disks && cli.mounts.use_mft(root.as_deref().unwrap_or(&path)),
            threads: None,
            job: cli.mounts.job(),
            rules: cli.rules.load(),
        };
        // The cache is for a directory named on the command line, not one read from a file or
        // chosen from the disks.
        let slot = (resumed.is_none() && !cli.disks && !cli.no_cache)
            .then(|| cache_slot(&path, &local))
            .flatten();
        let start: StartLocal = Box::new(move |path, opts| local.start(path, opts));
        match (resumed, slot) {
            (Some((tree, journal)), _) => Source::Resumed {
                tree,
                journal,
                start,
            },
            (None, Some(slot)) => Source::Cached { path, slot, start },
            (None, None) => Source::Local {
                path: (!cli.disks).then_some(path),
                start,
            },
        }
    };
    let opts = dfdu_tui::RunOptions {
        source,
        engine,
        history: !cli.no_history && cli.import.is_none(),
        ui: UiSettings {
            theme,
            color: cli.color.0,
            bars: cli.bars,
            mouse: !cli.no_mouse,
            debug_stats: cli.debug_stats,
            types: cli.types.unwrap_or_default(),
        },
    };
    match dfdu_tui::run(opts) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("dfdu: {e}");
            Err(ExitCode::from(1))
        }
    }
}

/// Where the scan of `path` with the options of `local` is cached; `None` when the cache is
/// off ([`dfdu_store::cache::OFF_VAR`]), refused (said on standard error before the browser
/// takes the terminal: another user's directory, or one others can write to), or `path` is not
/// a directory (the scan then says why).
fn cache_slot(path: &Path, local: &Local) -> Option<dfdu_store::cache::Slot> {
    let cache = match dfdu_store::cache::Cache::open_default()? {
        Ok(cache) => cache,
        Err(e) => {
            eprintln!("dfdu: {e}; the scan cache is not used");
            return None;
        }
    };
    let key =
        dfdu_store::cache::Key::new(path, local.job.cross_mounts, local.job.network, local.mft)
            .ok()?;
    Some(cache.slot(key))
}

/// The theme `--theme` names: `auto`, a built-in theme, or the theme file `NAME.toml` in
/// `themes` (`None` when the platform has no configuration directory).
fn theme_choice(name: &str, themes: Option<&Path>) -> Result<ThemeChoice, String> {
    if let Some(choice) = ThemeChoice::builtin(name) {
        return Ok(choice);
    }
    let builtin = format!("auto, {}", Theme::builtin_names().join(", "));
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
    Ok(ThemeChoice::File(file))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn bad_theme_file_is_reported() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let themes = tmp.path();
        let write = |name: &str, text: &str| fs::write(themes.join(name), text).expect("write");
        write("broken.toml", "base = \"sepia\"\n");
        write("typo.toml", "[tokens]\nfg = \"#12345\"\n");
        write("syntax.toml", "base = \n");
        write(
            "nord.toml",
            "base = \"light\"\n[tokens]\nfg = \"#2e3440\"\n",
        );
        fs::create_dir(themes.join("dir.toml")).expect("mkdir");

        let err = |name: &str| theme_choice(name, Some(themes)).expect_err(name);
        let missing = err("missing");
        assert!(missing.contains("not a built-in theme"), "{missing}");
        assert!(missing.contains("missing.toml"), "{missing}");
        let broken = err("broken");
        assert!(
            broken.contains("broken.toml") && broken.contains("sepia"),
            "{broken}"
        );
        let typo = err("typo");
        assert!(typo.contains("`fg`") && typo.contains("#12345"), "{typo}");
        assert!(err("syntax").contains("syntax.toml"));
        assert!(err("dir").contains("dir.toml"));
        assert!(err("../nord").contains("not a built-in theme"));
        assert!(theme_choice("nord", None).is_err());

        match theme_choice("nord", Some(themes)) {
            Ok(ThemeChoice::File(file)) => {
                assert_eq!(file.name.as_deref(), Some("nord"));
                assert_eq!(file.base(), dfdu_tui::BuiltinTheme::Light);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            theme_choice("high-contrast", None),
            Ok(ThemeChoice::Builtin(dfdu_tui::BuiltinTheme::HighContrast))
        ));
        assert!(matches!(theme_choice("auto", None), Ok(ThemeChoice::Auto)));
    }
}
