//! The filter of the current directory (`/`): hides the rows whose names do not match.

use std::time::Instant;

use ratatui::crossterm::event::KeyEvent;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect};
use crate::snapshot::{EntryData, Snapshot};

#[derive(Clone, Debug, Default)]
pub struct Filter {}

impl Filter {
    /// Brings the matches up to date with `snap.entries` before the rows are sorted or synced
    /// (not for frames whose rows stay as they are). `replaced`: the entries are not those of
    /// the last call with new ones appended (another directory, or a rescan replaced some), so
    /// what was computed per index no longer applies.
    pub fn prepare(&mut self, _snap: &Snapshot, _replaced: bool) {}

    /// Whether entry `index` (`e`) passes the filter; a lookup into what [`Filter::prepare`]
    /// computed.
    pub fn matches(&self, _index: usize, _e: &EntryData) -> bool {
        true
    }
}

/// Starts editing the filter.
pub fn open(_app: &mut App) {}

/// True while the filter input takes the keys.
pub fn editing(_app: &App) -> bool {
    false
}

pub fn key(_app: &mut App, _k: KeyEvent, _now: Instant) -> Vec<Effect> {
    Vec::new()
}

/// Clears an active filter. False if there was none.
pub fn clear(_app: &mut App) -> bool {
    false
}

/// The filter's line above the status line, while it has one.
pub fn line(_app: &App) -> Option<Line<'static>> {
    None
}

/// The name of entry `index` drawn in `style`, with the matched characters highlighted.
pub fn name_spans(_app: &App, _index: usize, name: String, style: Style) -> Vec<Span<'static>> {
    vec![Span::styled(name, style)]
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use crate::app::keys::tests::{keys_change_nothing, press};

    #[test]
    fn slash_changes_nothing() {
        keys_change_nothing(&[press(KeyCode::Char('/'))]);
    }
}
