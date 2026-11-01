//! Mark by pattern: a glob over the names the list shows in the current directory, such as
//! `*.log` or `IMG_????.jpg`. Globs are those of the rules ([`dfdu_rules::Glob`]): `*` and `?`,
//! no `[...]`, and case ignored on macOS and Windows, where file names usually ignore it. A
//! front end keeps the prompt; what the glob typed matches is counted as it is typed
//! ([`count`]), and [`mark`] marks it.

use dfdu_rules::{Glob, GlobError, Platform};

use crate::format::human;
use crate::marks;
use crate::snapshot::EntryData;
use crate::state::SessionState;

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

/// The entries the list of `s` shows that `g` matches ([`matched`]). Directories are left out
/// while a type filter shows only some of their files.
pub fn matched_in(s: &SessionState, g: &Glob) -> Vec<usize> {
    let folders = !crate::type_filter::active(s);
    matched(&s.snap.entries, &s.list.rows, folders, g)
}

/// How many names the glob of `text` matches, or what is wrong with it; `None` while nothing
/// is typed.
pub fn count(s: &SessionState, text: &str) -> Option<Result<usize, GlobError>> {
    let text = text.trim();
    (!text.is_empty()).then(|| glob(text).map(|g| matched_in(s, &g).len()))
}

/// Marks the entries `pattern` matches, and says how many on the status line.
pub fn mark(s: &mut SessionState, pattern: &str) {
    let g = match glob(pattern) {
        Ok(g) => g,
        Err(e) => {
            s.status = Some(format!("mark by pattern: {e}"));
            return;
        }
    };
    let hits = matched_in(s, &g);
    if hits.is_empty() {
        s.status = Some(format!("nothing here matches {pattern}"));
        return;
    }
    let items: Vec<marks::Item> = (hits.iter()).map(|&i| marks::item_of(s, i)).collect();
    let size: u64 = items.iter().map(|it| it.size).sum();
    let n = items.len();
    let added = marks::add(s, items);
    s.status = Some(match added {
        0 => format!("everything matching {pattern} is marked already"),
        _ => format!(
            "marked {added} of {n} entr{} matching {pattern} ({})",
            if n == 1 { "y" } else { "ies" },
            human(size)
        ),
    });
}
