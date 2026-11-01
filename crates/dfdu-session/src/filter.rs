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
//! The front end draws the filter's line and highlights the matched characters of names from
//! what this keeps ([`Filter::input`], [`Filter::shown`], [`Filter::indices`]). It sorts the
//! rows again after an edit ([`Edited`]), as the order is brought up to date with its frames.

use std::cell::{Ref, RefCell};

use dfdu_tree::NodeRef;

use crate::fuzzy::Fuzzy;
use crate::line::{Edit, LineInput, TextOp};
use crate::snapshot::{EntryData, Snapshot};
use crate::state::{Selection, SessionState};

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
pub fn open(s: &mut SessionState) {
    let f = &mut s.filter;
    f.editing = true;
    let text = f.input.text().to_owned();
    f.input.set(&text);
    f.counts = f.count(&s.snap.entries);
    s.keep_exact_order();
}

/// True while the filter input takes the keys.
pub fn editing(s: &SessionState) -> bool {
    s.filter.editing
}

/// What the front end does after an edit of the query ([`edit`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Edited {
    /// Nothing more.
    Done,
    /// The query changed: the rows it leaves are needed at once. The front end notes the
    /// selected entry ([`selected_node`]), asks for them ([`SessionState::request_refilter`]),
    /// brings the rows up to date, then passes the entry noted to [`refiltered`].
    Refilter,
    /// The only row left, a directory, is selected: the front end opens it as `Enter` does.
    Enter,
}

/// An edit of the query while editing: typing filters at once, [`TextOp::Submit`] keeps the
/// filter and gives the keys back to the list (and opens the only entry left, when that is a
/// directory, as nnn does), [`TextOp::Cancel`] clears it.
pub fn edit(s: &mut SessionState, op: TextOp) -> Edited {
    match s.filter.input.apply(op) {
        Edit::Changed => return Edited::Refilter,
        Edit::Submit => {
            s.filter.editing = false;
            if s.filter.input.text().trim().is_empty() {
                clear(s);
            } else if let [only] = s.list.rows[..]
                && s.snap.entries[only].is_dir
                && !s.snap.entries[only].pseudo
            {
                s.select(0);
                return Edited::Enter;
            }
        }
        Edit::Cancel => {
            clear(s);
        }
        Edit::Moved | Edit::Ignored => {}
    }
    Edited::Done
}

/// After the rows of a changed query came in ([`Edited::Refilter`]), with the entry selected
/// `before` them: an entry the query hid is remembered, and one remembered comes back when it
/// shows again.
pub fn refiltered(s: &mut SessionState, before: Option<NodeRef>) {
    let after = selected_node(s);
    let row_of = |s: &SessionState, node| {
        s.list
            .rows
            .iter()
            .position(|&i| s.snap.entries[i].node == node)
    };
    s.filter.restore = match s.filter.restore {
        Some((hidden, moved_to)) if before == moved_to => match row_of(s, hidden) {
            Some(row) => {
                s.select(row);
                None
            }
            None => Some((hidden, after)),
        },
        _ => match before {
            Some(b) if after != before && row_of(s, b).is_none() => Some((b, after)),
            _ => None,
        },
    };
}

/// The selected entry's reference.
pub fn selected_node(s: &SessionState) -> Option<NodeRef> {
    s.selected_entry().map(|e| e.node)
}

/// Clears an active filter. False if there was none. The entry the filter hid from the
/// selection is selected again, unless the user picked another one since.
pub fn clear(s: &mut SessionState) -> bool {
    let f = &mut s.filter;
    let had = f.editing || !f.input.is_empty();
    f.editing = false;
    if f.input.is_empty() {
        return had;
    }
    f.input.clear();
    s.request_refilter();
    if let Some((hidden, moved_to)) = s.filter.restore.take()
        && selected_node(s) == moved_to
        && let Some(e) = s.snap.entries.iter().find(|e| e.node == hidden)
        && !e.ghost
        && !(s.hide_hidden && e.hidden())
    {
        s.selected = Some(Selection::of(e));
    }
    had
}

#[cfg(test)]
mod tests;
