//! The cleanup view (`3`): every match of the rules, grouped by category
//! and safety, with the reclaimable total of each level. A group opens to its items (size,
//! entries, age, path); the selected item's explanation and what deleting it loses are shown
//! below. Marks are the list's: `d` and `D` delete what is marked through the same dialog.
//! The first time the user opens the view and it has items, it marks the recommended ones:
//! `safe` items above their rule's minimum size, `regenerable` ones only when stale, never
//! `review` or worse, and nothing changed in the last 7 days or in use. A read-only browser
//! marks nothing, as nothing marked could be deleted. `n` opens an item's native command: its dry
//! run first, then `y` runs it; `v` picks a further-reaching form, which `y` must confirm
//! twice.

use std::collections::HashSet;
use std::path::PathBuf;

use dfdu_engine::{Command, Model, Verdict, verdict};
use dfdu_rules::Safety;
use dfdu_tree::{Class, DirState, Ix, Kind, NodeRef, Totals, now_secs};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect};
use crate::model::snapshot::{Ident, Target};
use crate::state::marks;
use crate::theme::human;

mod draw;
mod pane;

pub use draw::draw;

/// Output lines the native pane keeps.
const PANE_LINES: usize = 200;

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Cleanup {
    pub on: bool,
    cursor: usize,
    scroll: usize,
    /// Groups shown with their items, by (category, safety).
    open: HashSet<(u16, Safety)>,
    /// Only items this safe or safer; `None` shows all.
    filter: Option<Safety>,
    /// The recommended items were marked (once, when the view was first opened with items).
    preselected: bool,
    /// The view was just opened: the next read marks the recommended items if it finds any
    /// matches, and they were never marked before.
    premark: bool,
    pane: Option<pane::Pane>,
    commands: u64,
    /// The extras were read since the view was switched on.
    fresh: bool,
}

/// One match, as the last read found it.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeRef,
    pub path: PathBuf,
    pub dev_ino: (u64, u64),
    pub kind: Kind,
    pub class: Class,
    pub size: u64,
    pub items: u64,
    pub shared_out: u64,
    pub mtime: i64,
    pub scanning: bool,
    /// With matches inside it listed on their own, the entries that marking it marks
    /// ([`dfdu_engine::Listed::rest`]); empty when it is marked whole.
    pub rest: Vec<Item>,
}

/// The native command output the view shows.
#[derive(Clone, Debug, Default)]
pub struct NativeView {
    pub id: u64,
    pub command: String,
    pub lines: Vec<String>,
    pub done: Option<Result<(), String>>,
}

/// What [`read`] takes from the model.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub items: Vec<Item>,
    pub native: Option<NativeView>,
    /// What native size commands reported, by rule name.
    pub native_sizes: Vec<(String, Result<u64, String>)>,
}

/// True when the view is shown, and so takes the keys it knows.
pub fn active(app: &App) -> bool {
    app.cleanup.on && app.snap.rules.is_some()
}

/// Switches the view on or off. On, it asks the rules' native size commands for their
/// numbers, and the first time also marks the recommended items ([`apply`]).
pub fn show(app: &mut App, on: bool) -> Vec<Effect> {
    if on && app.snap.rules.is_none() {
        app.status = Some("no rules: the cleanup view needs classification".into());
        return Vec::new();
    }
    let was = app.cleanup.on;
    app.cleanup.on = on;
    app.cleanup.fresh = false;
    app.cleanup.premark = on && !app.cleanup.preselected && !app.read_only;
    if on && !was {
        crate::features::treemap::show(app, false);
        return vec![Effect::Send(Command::NativeSizes)];
    }
    Vec::new()
}

/// The view needs a read: it was just switched on.
pub fn stale(app: &App) -> bool {
    app.cleanup.on && !app.cleanup.fresh
}

/// The listed matches ([`dfdu_engine::listed`]), under the read lock. Matches are few: one per
/// matched directory, and below it only those a rule that does not stop the descent lets in.
pub fn read(m: &Model, app: &App) -> Info {
    if !app.cleanup.on {
        return Info::default();
    }
    let t = &m.tree;
    let item = |ix: Ix, path: PathBuf, class: Class, tot: Totals, scanning: bool| Item {
        node: t.node_ref(ix),
        path,
        dev_ino: t.dev_ino(ix),
        kind: t.kind(ix),
        class,
        size: tot.alloc,
        items: tot.items,
        shared_out: tot.shared_out,
        mtime: t.mtime(ix),
        scanning,
        rest: Vec::new(),
    };
    let listed = match &m.rules {
        Some(rules) => dfdu_engine::listed(t, rules),
        None => Vec::new(),
    };
    let items: Vec<Item> = listed
        .into_iter()
        .map(|l| {
            let rest = (l.rest.iter())
                .map(|&r| {
                    let scanning = t.dir(r).is_some_and(|d| d.state == DirState::Pending);
                    item(r, t.path_buf(r), l.class, t.total(r), scanning)
                })
                .collect();
            Item {
                rest,
                ..item(l.ix, l.path, l.class, l.total, l.scanning)
            }
        })
        .collect();
    let native = m.status.native.as_ref().map(|n| NativeView {
        id: n.id,
        command: n.command.clone(),
        lines: n
            .lines
            .iter()
            .skip(n.lines.len().saturating_sub(PANE_LINES))
            .cloned()
            .collect(),
        done: n.done.clone(),
    });
    let mut native_sizes: Vec<(String, Result<u64, String>)> = match &m.rules {
        Some(rules) => m
            .native_sizes
            .iter()
            .map(|(&r, v)| (rules.rule(r).name.clone(), v.clone()))
            .collect(),
        None => vec![],
    };
    native_sizes.sort_by(|a, b| a.0.cmp(&b.0));
    Info {
        items,
        native,
        native_sizes,
    }
}

/// Takes in a read. The first read after the user opened the view ([`show`]) marks the
/// recommended items, unless it found no matches yet: then the next opening tries again.
pub fn apply(app: &mut App) {
    app.cleanup.fresh = true;
    let premark = std::mem::take(&mut app.cleanup.premark);
    if !app.cleanup.on || !premark || app.extras.cleanup.items.is_empty() {
        return;
    }
    app.cleanup.preselected = true;
    let (n, size) = mark_recommended(app);
    if n > 0 {
        app.status = Some(marked_status(n, size));
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The status line after `n` recommended items of `size` in all were marked.
fn marked_status(n: usize, size: u64) -> String {
    format!(
        "marked {n} recommended item{} ({}) · Space unmarks",
        plural(n),
        human(size)
    )
}

/// Whether `it` is marked by default, and why; `None` without rules.
fn recommended(app: &App, it: &Item, now: i64) -> Option<Verdict> {
    let rules = app.snap.rules.as_ref()?;
    Some(verdict(
        rules.rule(it.class.rule),
        it.class,
        it.size,
        it.mtime,
        it.scanning,
        now,
    ))
}

/// Marks every recommended item; returns how many were newly marked, and their size.
fn mark_recommended(app: &mut App) -> (usize, u64) {
    let now = now_secs();
    let picks: Vec<Item> = app
        .extras
        .cleanup
        .items
        .iter()
        .filter(|it| recommended(app, it, now).is_some_and(Verdict::recommended))
        .filter(|it| !is_marked(app, it))
        .cloned()
        .collect();
    for it in &picks {
        set_marked(app, it, true);
    }
    let size = picks.iter().map(marked_size).sum();
    (picks.len(), size)
}

/// What marking `it` marks: itself, or its rest.
fn marked_size(it: &Item) -> u64 {
    if it.rest.is_empty() {
        it.size
    } else {
        it.rest.iter().map(|r| r.size).sum()
    }
}

/// Whether `it` is marked: itself, or all of its rest.
fn is_marked(app: &App, it: &Item) -> bool {
    if it.rest.is_empty() {
        app.marks.contains(it.node)
    } else {
        it.rest.iter().all(|r| app.marks.contains(r.node))
    }
}

/// Marks or unmarks `it`, or each entry of its rest.
fn set_marked(app: &mut App, it: &Item, on: bool) {
    if it.rest.is_empty() {
        marks::set(app, mark_of(it), on);
    }
    for r in &it.rest {
        marks::set(app, mark_of(r), on);
    }
}

fn mark_of(it: &Item) -> marks::Item {
    marks::Item {
        target: Target {
            node: Some(it.node),
            path: it.path.clone(),
            ident: Some(Ident {
                dev_ino: it.dev_ino,
                kind: it.kind,
            }),
        },
        is_dir: it.kind == Kind::Dir,
        size: it.size,
        items: it.items,
        shared_out: it.shared_out,
        scanning: it.scanning,
        inside: false,
    }
}

/// Protected matches (git-tracked ones) are listed but cannot be marked.
fn markable(it: &Item) -> bool {
    it.class.safety < Safety::Protected
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Row {
    Group { cat: u16, safety: Safety },
    Item(usize),
}

/// The rows: groups in order of safety, then size; each open group followed by its items.
fn rows(app: &App) -> Vec<Row> {
    let Some(rules) = &app.snap.rules else {
        return vec![];
    };
    let items = &app.extras.cleanup.items;
    let mut groups: Vec<((u16, Safety), u64)> = vec![];
    for it in items {
        if app.cleanup.filter.is_some_and(|f| it.class.safety > f) {
            continue;
        }
        let key = (rules.rule(it.class.rule).category, it.class.safety);
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1 += it.size,
            None => groups.push((key, it.size)),
        }
    }
    groups.sort_by(|a, b| a.0.1.cmp(&b.0.1).then(b.1.cmp(&a.1)));
    let mut out = vec![];
    for ((cat, safety), _) in groups {
        out.push(Row::Group { cat, safety });
        if app.cleanup.open.contains(&(cat, safety)) {
            out.extend(
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| {
                        it.class.safety == safety && rules.rule(it.class.rule).category == cat
                    })
                    .map(|(i, _)| Row::Item(i)),
            );
        }
    }
    out
}

fn group_items(app: &App, cat: u16, safety: Safety) -> Vec<usize> {
    let Some(rules) = &app.snap.rules else {
        return vec![];
    };
    (app.extras.cleanup.items.iter().enumerate())
        .filter(|(_, it)| it.class.safety == safety && rules.rule(it.class.rule).category == cat)
        .map(|(i, _)| i)
        .collect()
}

pub(crate) fn selected_item(app: &App) -> Option<&Item> {
    match rows(app).get(app.cleanup.cursor)? {
        Row::Item(i) => app.extras.cleanup.items.get(*i),
        Row::Group { .. } => None,
    }
}

/// Keys the view takes before the action registry; `None` passes the key on.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    if app.cleanup.pane.is_some() {
        return Some(pane::key(app, k));
    }
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let c = &mut app.cleanup;
    if let Some(n) = crate::app::keys::half_page(k, 10) {
        c.cursor = c.cursor.saturating_add_signed(n).min(last);
        return Some(Vec::new());
    }
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => c.cursor = c.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => c.cursor = (c.cursor + 1).min(last),
        KeyCode::PageUp => c.cursor = c.cursor.saturating_sub(10),
        KeyCode::PageDown => c.cursor = (c.cursor + 10).min(last),
        KeyCode::Home | KeyCode::Char('g') => c.cursor = 0,
        KeyCode::End | KeyCode::Char('G') => c.cursor = last,
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match rows.get(c.cursor) {
            Some(&Row::Group { cat, safety }) => {
                if !c.open.remove(&(cat, safety)) {
                    c.open.insert((cat, safety));
                }
            }
            Some(Row::Item(_)) => return Some(pane::open_pane(app)),
            None => {}
        },
        KeyCode::Left | KeyCode::Char('h') => {
            let at = (0..=c.cursor.min(last))
                .rev()
                .find(|&i| matches!(rows.get(i), Some(Row::Group { .. })));
            if let Some(i) = at
                && let Row::Group { cat, safety } = rows[i]
            {
                c.open.remove(&(cat, safety));
                c.cursor = i;
            }
        }
        KeyCode::Char(' ') => toggle(app, rows.get(app.cleanup.cursor).copied()),
        KeyCode::Char('f') => {
            c.filter = match c.filter {
                None => Some(Safety::Safe),
                Some(Safety::Safe) => Some(Safety::Regenerable),
                Some(Safety::Regenerable) => Some(Safety::Review),
                Some(_) => None,
            };
            c.cursor = 0;
            let label = c.filter.map_or("everything", |f| match f {
                Safety::Safe => "safe only",
                Safety::Regenerable => "safe and regenerable",
                _ => "up to review",
            });
            app.status = Some(format!("cleanup: showing {label}"));
        }
        KeyCode::Char('a') if app.read_only => {
            app.status = Some(crate::app::keys::READ_ONLY.into());
        }
        KeyCode::Char('a') => {
            let (n, size) = mark_recommended(app);
            app.status = Some(if n == 0 {
                "every recommended item is marked already".into()
            } else {
                marked_status(n, size)
            });
        }
        KeyCode::Char('n') => return Some(pane::open_pane(app)),
        _ => return None,
    }
    Some(Vec::new())
}

fn toggle(app: &mut App, row: Option<Row>) {
    let picks: Vec<usize> = match row {
        Some(Row::Item(i)) => vec![i],
        Some(Row::Group { cat, safety }) => group_items(app, cat, safety),
        None => return,
    };
    let items: Vec<Item> = picks
        .iter()
        .filter_map(|&i| app.extras.cleanup.items.get(i).cloned())
        .filter(markable)
        .collect();
    if items.is_empty() {
        app.status = Some("protected: cannot be marked".into());
        return;
    }
    let on = !items.iter().all(|it| is_marked(app, it));
    for it in &items {
        set_marked(app, it, on);
    }
    if matches!(row, Some(Row::Item(_))) {
        app.cleanup.cursor += 1;
    }
}

impl Cleanup {
    /// Keeps the cursor on one of `rows` rows and inside the `list_h` rows shown.
    fn clamp_scroll(&mut self, rows: usize, list_h: usize) {
        self.cursor = self.cursor.min(rows.saturating_sub(1));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if list_h > 0 && self.cursor >= self.scroll + list_h {
            self.scroll = self.cursor + 1 - list_h;
        }
    }
}

#[cfg(test)]
mod tests;
