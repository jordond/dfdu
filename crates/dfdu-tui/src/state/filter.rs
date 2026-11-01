//! The filter of the current directory (`/`): hides the rows whose names do not match.
//!
//! The query is matched fuzzily ([`Fuzzy`]) against each name as it is drawn (lossy UTF-8).
//! Whether each entry matches is kept per index of the snapshot's entries, so new entries of a
//! running scan are tested as they arrive, and a query that only grew re-tests only the entries
//! that matched before. The filter only hides rows: the rest keep their sort order.
//!
//! In tree mode it matches anywhere in the expanded tree, and the rows a match is inside stay
//! to show where it is. Fold rows never match.
//!
//! The view draws the filter's line and highlights the matched characters of names from what
//! this keeps ([`Filter::input`], [`Filter::shown`], [`Filter::indices`]).

use std::cell::{Ref, RefCell};
use std::time::Instant;

use dfdu_tree::NodeRef;

use crate::app::{App, Effect, Selection};
use crate::input::fuzzy::Fuzzy;
use crate::input::line::{Edit, LineInput, TextOp};
use crate::model::snapshot::{EntryData, Snapshot};

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
    /// Matcher for `query`; a `RefCell` so drawing can highlight with it ([`Filter::indices`]).
    fuzzy: RefCell<Fuzzy>,
    /// `hits[i]`: entry `i` matches `query`. Empty while the query has no words.
    hits: Vec<bool>,
    /// `around[i]`: in tree mode, some match is inside entry `i`. Empty otherwise.
    around: Vec<bool>,
    /// Directory the entries are from; another one clears the filter.
    dir: dfdu_tree::HostPath,
    counts: Counts,
    /// The selected entry when the query hid it, and the entry the selection moved to then
    /// (`None`: no row was left). While that one stays selected, the selection goes back as
    /// soon as the hidden entry shows again.
    restore: Option<(NodeRef, Option<NodeRef>)>,
    /// Buffer for the matched character indices of a drawn name ([`Filter::indices`]).
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
            if !self.dir.is_empty() {
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
                    *hit = *hit && hits(fuzzy, e);
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
            self.hits
                .extend(snap.entries[from..].iter().map(|e| hits(fuzzy, e)));
        } else {
            self.hits.clear();
        }
        self.around.clear();
        if self.active && snap.tree.is_some() {
            self.around = around(&snap.entries, &self.hits);
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
            let (hidden, hit) = (e.hidden(), !self.active || self.hit(i));
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

    /// The query in force, for the header while the filter is kept.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether entry `index` passes the filter: it matches, or in tree mode a match is inside
    /// it. A lookup into what [`Filter::prepare`] computed.
    pub fn matches(&self, index: usize) -> bool {
        !self.active || self.hit(index) || self.around.get(index).copied().unwrap_or(false)
    }

    fn hit(&self, index: usize) -> bool {
        self.hits.get(index).copied().unwrap_or(false)
    }

    /// The input line of the query.
    pub fn input(&self) -> &LineInput {
        &self.input
    }

    /// How many entries pass the filter and how many there are, ghosts left out, and hidden
    /// files too when `hide_hidden`: for the filter's line.
    pub fn shown(&self, hide_hidden: bool) -> (usize, usize) {
        let c = self.counts;
        if hide_hidden {
            (c.matched - c.matched_hidden, c.entries - c.hidden)
        } else {
            (c.matched, c.entries)
        }
    }

    /// The positions of the characters of `name` the query matches, in order, to highlight
    /// them; `None` while the filter is not active or `name` does not match.
    pub fn indices(&self, name: &str) -> Option<Ref<'_, [u32]>> {
        if !self.active {
            return None;
        }
        let mut indices = self.indices.borrow_mut();
        if self
            .fuzzy
            .borrow_mut()
            .indices(name, &mut indices)
            .is_none()
            || indices.is_empty()
        {
            return None;
        }
        drop(indices);
        Some(Ref::map(self.indices.borrow(), Vec::as_slice))
    }
}

/// Whether `e`'s name matches; a fold row's text is not a name.
fn hits(fuzzy: &mut Fuzzy, e: &EntryData) -> bool {
    e.fold == 0 && fuzzy.matches(&String::from_utf8_lossy(&e.name))
}

/// For each entry of a tree read, whether one of `hits` is inside it. A row comes after the
/// row it is in, so one pass from the end carries each match up to its parent.
fn around(entries: &[EntryData], hits: &[bool]) -> Vec<bool> {
    let mut out = vec![false; entries.len()];
    for i in (0..entries.len()).rev() {
        let p = entries[i].parent as usize;
        if p < i && (out[i] || hits.get(i).copied().unwrap_or(false)) {
            out[p] = true;
        }
    }
    out
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

/// An edit of the query while editing: typing filters at once, [`TextOp::Submit`] keeps the
/// filter and gives the keys back to the list (and opens the only entry left, when that is a
/// directory, as nnn does), [`TextOp::Cancel`] clears it.
pub fn edit(app: &mut App, op: TextOp, now: Instant) -> Vec<Effect> {
    match app.filter.input.apply(op) {
        Edit::Changed => refilter(app, now),
        Edit::Submit => {
            app.filter.editing = false;
            if app.filter.input.text().trim().is_empty() {
                clear(app);
            } else if let [only] = app.list.rows[..]
                && app.snap.entries[only].is_dir
                && !app.snap.entries[only].pseudo
            {
                app.select(0);
                return crate::app::keys::run(app, crate::app::keys::Action::Enter);
            }
        }
        Edit::Cancel => {
            clear(app);
        }
        Edit::Moved | Edit::Ignored => {}
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
        app.list
            .rows
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
        app.selected = Some(Selection::of(e));
    }
    had
}

#[cfg(test)]
mod tests;
