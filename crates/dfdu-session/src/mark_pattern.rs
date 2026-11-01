//! Mark by pattern: a glob over the names the list shows in the current directory, such as
//! `*.log` or `IMG_????.jpg`. Globs are those of the rules ([`dfdu_rules::Glob`]): `*` and `?`,
//! no `[...]`, and case ignored on macOS and Windows, where file names usually ignore it.

use dfdu_rules::{Glob, GlobError, Platform};

use crate::snapshot::EntryData;

/// The glob of `pattern`, compared as file names are on this platform.
pub fn glob(pattern: &str) -> Result<Glob, GlobError> {
    let fold = matches!(Platform::current(), Platform::MacOs | Platform::Windows);
    Glob::new(pattern.trim(), fold)
}

/// The entries of `rows` (indices into `entries`, the rows the list shows) that `g` matches.
/// The "shared between entries" row is not an entry, and directories are left out without
/// `folders` (while a type filter shows only some of their files).
pub fn matched(entries: &[EntryData], rows: &[usize], folders: bool, g: &Glob) -> Vec<usize> {
    (rows.iter().copied())
        .filter(|&i| {
            let e = &entries[i];
            !e.pseudo && (folders || !e.is_dir) && g.matches(&e.name)
        })
        .collect()
}
