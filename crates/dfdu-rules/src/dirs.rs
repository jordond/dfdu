//! Where dfdu keeps its files: the configuration directory (rule files, themes), the state
//! directory (deletion journals, staging, crash reports, scan history) and the cache directory
//! (the scan cache, the downloaded agents). XDG directories on Linux and macOS, Known Folders on Windows (which has no
//! state directory, so the data directory is used there); the caches (scans, and the agents
//! downloaded for `--remote` hosts) are in the platform's own cache directory
//! (`~/Library/Caches` on macOS). Every path is `None` without a home directory.

use std::path::PathBuf;

use etcetera::BaseStrategy;

/// The environment variable that replaces the state directory (used as given, without a
/// `dfdu` folder of its own).
pub const STATE_DIR_VAR: &str = "DFDU_STATE_DIR";

/// The environment variable that replaces the scan cache directory (used as given).
pub const CACHE_DIR_VAR: &str = "DFDU_CACHE_DIR";

/// `dfdu` in the configuration directory (the roaming `AppData` folder on Windows).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let dirs = etcetera::choose_base_strategy().ok()?;
    Some(dirs.config_dir().join("dfdu"))
}

/// Where the user's rule files are: `rules` in [`config_dir`].
#[must_use]
pub fn rules_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("rules"))
}

/// Where the user's theme files are: `themes` in [`config_dir`].
#[must_use]
pub fn theme_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("themes"))
}

/// `$DFDU_STATE_DIR`, else `dfdu` in the platform state directory.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(STATE_DIR_VAR) {
        return Some(PathBuf::from(d));
    }
    let dirs = etcetera::choose_base_strategy().ok()?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_dir());
    Some(base.join("dfdu"))
}

/// The scan cache: `$DFDU_CACHE_DIR`, else `dfdu/scans` in the platform cache directory
/// (`~/Library/Caches` on macOS, `$XDG_CACHE_HOME` or `~/.cache` on Linux, the local
/// `AppData` folder on Windows).
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(CACHE_DIR_VAR) {
        return Some(PathBuf::from(d));
    }
    let dirs = etcetera::base_strategy::choose_native_strategy().ok()?;
    Some(dirs.cache_dir().join("dfdu").join("scans"))
}

/// The release agents downloaded for `--remote` hosts: `dfdu/agents` in the platform cache
/// directory.
#[must_use]
pub fn agent_cache_dir() -> Option<PathBuf> {
    let dirs = etcetera::base_strategy::choose_native_strategy().ok()?;
    Some(dirs.cache_dir().join("dfdu").join("agents"))
}

/// The deletion journals: `ops` in [`state_dir`].
#[must_use]
pub fn journal_dir() -> Option<PathBuf> {
    Some(state_dir()?.join("ops"))
}

/// Crash reports: `crash` in [`state_dir`].
#[must_use]
pub fn crash_dir() -> Option<PathBuf> {
    Some(state_dir()?.join("crash"))
}

/// The scan history database: `history.sqlite3` in [`state_dir`].
#[must_use]
pub fn history_path() -> Option<PathBuf> {
    Some(state_dir()?.join("history.sqlite3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_files_share_the_state_directory() {
        let Some(state) = state_dir() else { return };
        assert_eq!(journal_dir(), Some(state.join("ops")));
        assert_eq!(crash_dir(), Some(state.join("crash")));
        assert_eq!(history_path(), Some(state.join("history.sqlite3")));
    }

    #[test]
    fn the_cache_is_apart_from_the_state() {
        if std::env::var_os(CACHE_DIR_VAR).is_some() {
            return;
        }
        let Some(cache) = cache_dir() else { return };
        assert!(cache.ends_with("dfdu/scans"), "{}", cache.display());
        assert_ne!(Some(cache.as_path()), state_dir().as_deref());
    }

    #[test]
    fn agents_are_cached_beside_the_scans() {
        let Some(agents) = agent_cache_dir() else {
            return;
        };
        assert!(agents.ends_with("dfdu/agents"), "{}", agents.display());
    }
}
