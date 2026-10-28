//! The filter of the current directory (`/`): hides the rows whose names do not match.
//!
//! The query is matched fuzzily ([`Fuzzy`]) against each name as it is drawn (lossy UTF-8).
//! Whether each entry matches is kept per index of the snapshot's entries, so new entries of a
//! running scan are tested as they arrive, and a query that only grew re-tests only the entries
//! that matched before. The filter only hides rows: the rest keep their sort order.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use dfdu_tree::NodeRef;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Effect, Selection};
use crate::fuzzy::{Fuzzy, char_pos};
use crate::input::{Edit, LineInput};
use crate::snapshot::{EntryData, Snapshot};

/// The query, its input line, and which entries of the current directory match it.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    input: LineInput,
    /// The input has the keys.
    editing: bool,
    /// The query `hits` holds the matches of.
    query: String,
    /// `query` has words; without any, everything matches.
    active: bool,
    /// Matcher for `query`; a `RefCell` so drawing can highlight with it.
    fuzzy: RefCell<Fuzzy>,
    /// `hits[i]`: entry `i` matches `query`. Empty while the query has no words.
    hits: Vec<bool>,
    /// Directory the entries are from; another one clears the filter.
    dir: PathBuf,
    counts: Counts,
    /// The selected entry when the query hid it, and the entry the selection moved to then
    /// (`None`: no row was left). While that one stays selected, the selection goes back as
    /// soon as the hidden entry shows again.
    restore: Option<(NodeRef, Option<NodeRef>)>,
    /// Buffer for the matched character indices of a drawn name.
    indices: RefCell<Vec<u32>>,
}

/// Entries counted for the filter's line, ghosts left out. Kept up to date by
/// [`Filter::prepare`] only while the filter is active or being edited, and counted when it opens.
#[derive(Copy, Clone, Debug, Default)]
struct Counts {
    entries: usize,
    hidden: usize,
    matched: usize,
    matched_hidden: usize,
}

impl Filter {
    /// Brings the matches up to date with `snap.entries` before the rows are sorted or synced
    /// (not for frames whose rows stay as they are). `replaced`: the entries are not those of
    /// the last call with new ones appended (another directory, or a rescan replaced some), so
    /// what was computed per index no longer applies.
    pub fn prepare(&mut self, snap: &Snapshot, replaced: bool) {
        if snap.dir_path != self.dir {
            // Before the first snapshot there is no directory, and a query typed by then stays.
            if !self.dir.as_os_str().is_empty() {
                self.input.clear();
                self.editing = false;
                self.restore = None;
            }
            self.dir.clone_from(&snap.dir_path);
        }
        let fuzzy = self.fuzzy.get_mut();
        if self.input.text() != self.query {
            // Every word only grew or was added: whatever matches now matched before. The new
            // query has at least the words of the old one, so it is not empty either.
            let narrowed =
                !replaced && !fuzzy.is_empty() && self.input.text().starts_with(&self.query);
            self.query = self.input.text().to_owned();
            *fuzzy = Fuzzy::new(&self.query);
            self.active = !fuzzy.is_empty();
            if narrowed {
                for (hit, e) in self.hits.iter_mut().zip(&snap.entries) {
                    *hit = *hit && fuzzy.matches(&String::from_utf8_lossy(&e.name));
                }
            } else {
                self.hits.clear();
            }
        } else if replaced {
            self.hits.clear();
        }
        if self.active {
            self.hits.truncate(snap.entries.len());
            let from = self.hits.len();
            self.hits.extend(
                snap.entries[from..]
                    .iter()
                    .map(|e| fuzzy.matches(&String::from_utf8_lossy(&e.name))),
            );
        } else {
            self.hits.clear();
        }
        if self.active || self.editing {
            self.counts = self.count(&snap.entries);
        }
    }

    /// Counts `entries` for the filter's line.
    fn count(&self, entries: &[EntryData]) -> Counts {
        let mut c = Counts::default();
        for (i, e) in entries.iter().enumerate() {
            if e.ghost {
                continue;
            }
            let (hidden, hit) = (e.hidden(), self.matches(i, e));
            c.entries += 1;
            c.hidden += usize::from(hidden);
            c.matched += usize::from(hit);
            c.matched_hidden += usize::from(hidden && hit);
        }
        c
    }

    /// The query has words, so the entries that do not match it are hidden.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Whether entry `index` (`e`) passes the filter; a lookup into what [`Filter::prepare`]
    /// computed.
    pub fn matches(&self, index: usize, _e: &EntryData) -> bool {
        !self.active || self.hits.get(index).copied().unwrap_or(false)
    }
}

/// Starts editing the filter, with the cursor after the query in force.
pub fn open(app: &mut App) {
    let f = &mut app.filter;
    f.editing = true;
    let text = f.input.text().to_owned();
    f.input.set(&text);
    f.counts = f.count(&app.snap.entries);
    app.keep_exact_order();
}

/// True while the filter input takes the keys.
pub fn editing(app: &App) -> bool {
    app.filter.editing
}

/// A key while editing: typing filters at once, `Enter` keeps the filter and gives the keys
/// back to the list, `Esc` clears it, `↑`/`↓` move the cursor in the list.
pub fn key(app: &mut App, k: KeyEvent, now: Instant) -> Vec<Effect> {
    match k.code {
        KeyCode::Up => app.move_by(-1),
        KeyCode::Down => app.move_by(1),
        _ => match app.filter.input.key(&k) {
            Edit::Changed => refilter(app, now),
            Edit::Submit => {
                app.filter.editing = false;
                if app.filter.input.text().trim().is_empty() {
                    clear(app);
                }
            }
            Edit::Cancel => {
                clear(app);
            }
            Edit::Moved | Edit::Ignored => {}
        },
    }
    Vec::new()
}

/// Applies a changed query to the rows at once, so the selection it moved is known: an entry
/// it hid is remembered, and one remembered comes back when it shows again.
fn refilter(app: &mut App, now: Instant) {
    let before = selected_node(app);
    app.request_refilter();
    app.reorder(now);
    let after = selected_node(app);
    let row_of = |app: &App, node| {
        app.rows
            .iter()
            .position(|&i| app.snap.entries[i].node == node)
    };
    app.filter.restore = match app.filter.restore {
        Some((hidden, moved_to)) if before == moved_to => match row_of(app, hidden) {
            Some(row) => {
                app.select(row);
                None
            }
            None => Some((hidden, after)),
        },
        _ => match before {
            Some(b) if after != before && row_of(app, b).is_none() => Some((b, after)),
            _ => None,
        },
    };
}

fn selected_node(app: &App) -> Option<NodeRef> {
    app.selected_entry().map(|e| e.node)
}

/// Clears an active filter. False if there was none. The entry the filter hid from the
/// selection is selected again, unless the user picked another one since.
pub fn clear(app: &mut App) -> bool {
    let f = &mut app.filter;
    let had = f.editing || !f.input.is_empty();
    f.editing = false;
    if f.input.is_empty() {
        return had;
    }
    f.input.clear();
    app.request_refilter();
    if let Some((hidden, moved_to)) = app.filter.restore.take()
        && selected_node(app) == moved_to
        && let Some(e) = app.snap.entries.iter().find(|e| e.node == hidden)
        && !e.ghost
        && !(app.hide_hidden && e.hidden())
    {
        app.selected = Some(Selection {
            node: Some(e.node),
            name: e.name.to_vec(),
            dev_ino: e.dev_ino,
        });
    }
    had
}

/// The filter's line above the status line, while it has one: the input while editing, else
/// the query in force.
pub fn line(app: &App) -> Option<Line<'static>> {
    let (f, t) = (&app.filter, &app.theme);
    if !f.editing && !f.active {
        return None;
    }
    let c = f.counts;
    let (shown, of) = if app.hide_hidden {
        (c.matched - c.matched_hidden, c.entries - c.hidden)
    } else {
        (c.matched, c.entries)
    };
    let spans = if f.editing {
        let mut spans = vec![Span::styled(" /", t.key), Span::raw(" ")];
        spans.extend(f.input.spans(Style::new(), t.key));
        spans.push(Span::styled(format!("   {shown} of {of}"), t.fg_dim));
        spans
    } else {
        vec![
            Span::styled(" filter: ", t.fg_dim),
            Span::raw(format!("\"{}\"", f.input.text())),
            Span::styled(format!(" ({shown} of {of})   "), t.fg_dim),
            Span::styled("/", t.key),
            Span::styled(" edit  ", t.fg_dim),
            Span::styled("Esc", t.key),
            Span::styled(" clear", t.fg_dim),
        ]
    };
    Some(Line::from(spans))
}

/// The name of entry `index` drawn in `style`, with the matched characters highlighted.
pub fn name_spans(app: &App, _index: usize, name: String, style: Style) -> Vec<Span<'static>> {
    let f = &app.filter;
    if !f.active {
        return vec![Span::styled(name, style)];
    }
    let mut indices = f.indices.borrow_mut();
    if f.fuzzy.borrow_mut().indices(&name, &mut indices).is_none() || indices.is_empty() {
        return vec![Span::styled(name, style)];
    }
    let matched = style.patch(app.theme.r#match);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    let mut next = indices.iter().copied().peekable();
    for (i, ch) in name.chars().enumerate() {
        let hit = next.next_if_eq(&char_pos(i)).is_some();
        if hit != run_hit && !run.is_empty() {
            let s = if run_hit { matched } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), s));
        }
        run_hit = hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { matched } else { style }));
    }
    spans
}

#[cfg(test)]
mod tests;
