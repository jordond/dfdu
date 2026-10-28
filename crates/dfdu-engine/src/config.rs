//! Where the user's configuration is.

use std::path::PathBuf;

/// `dfdu` in the configuration directory (XDG on Linux and macOS, the roaming `AppData` folder on
/// Windows). `None` without a home directory.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    use etcetera::BaseStrategy;
    let dirs = etcetera::choose_base_strategy().ok()?;
    Some(dirs.config_dir().join("dfdu"))
}

/// Where the user's rule files are (`dfdu-rules`): `rules` in [`config_dir`].
#[must_use]
pub fn rules_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("rules"))
}
