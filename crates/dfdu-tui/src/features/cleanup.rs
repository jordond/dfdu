//! The cleanup view (`3`): every match of the rules in the whole scan, one of the review views
//! (`features::review`). Sections by safety level (the decision being made), each saying what
//! the level means, hold the categories found at that level, which open to their items (size,
//! entries, age, path, and a one-word reason why they are suggested or not); levels without
//! matches are left out. The selected item's rule, what deleting it loses and the reason in full
//! are in the detail pane. Ticks are the list's marks: `d` and `D` delete what is ticked through
//! the same dialog, and `Enter` on an item shows it in Files.
//! The first time the user opens the view and it has items, it ticks the suggested ones:
//! `safe` items above their rule's minimum size, `regenerable` ones only when stale, never
//! `review` or worse, and nothing changed in the last 7 days or in use. A read-only browser
//! marks nothing, as nothing marked could be deleted. `N` opens an item's native command: its dry
//! run first, then `y` runs it; `v` picks a further-reaching form, which `y` must confirm
//! twice. `A` marks the recommended items again and `f` shows only the safer levels; like `N`,
//! they are registry actions (`app::keys`) that act only while the view is shown.

use std::collections::HashSet;

use dfdu_engine::{Command, Model, Verdict, verdict};
use dfdu_rules::Safety;
use dfdu_tree::{Class, DirState, HostPath, Ix, Kind, NodeRef, Totals, now_secs};
use ratatui::crossterm::event::KeyEvent;

use crate::app::keys::{self, Action};
use crate::app::{App, Effect};
use crate::model::snapshot::{Ident, Target};
use crate::state::marks;
use crate::theme::human;

mod draw;
mod pane;
mod rows;

pub use rows::enter_does;
pub(crate) use rows::selected_item;
use rows::{Row, group_items, is_open, rows, section_items, set_open};

pub use draw::draw;

/// Output lines the native pane keeps.
const PANE_LINES: usize = 200;

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Cleanup {
    pub on: bool,
    cursor: usize,
    scroll: usize,
    /// Categories shown with their items, by (category, safety); closed by default.
    open: HashSet<(u16, Safety)>,
    /// Safety sections closed by the user; open by default.
    closed: HashSet<Safety>,
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
    /// What the last read found to reclaim, for the view's tab; `None` before the first.
    reclaimable: Option<u64>,
    /// The tree's version and number of matches the last total was counted at.
    counted_at: Option<(u64, usize)>,
}

/// One match, as the last read found it.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeRef,
    pub path: HostPath,
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
    /// What the matches that can be ticked add up to, for the tab and the summary bar alike.
    pub total: Option<Reclaim>,
}

/// What the matches that can be ticked add up to.
#[derive(Clone, Copy, Debug)]
pub struct Reclaim {
    /// The tree's version and number of matches it was counted at ([`count_key`]).
    pub at: (u64, usize),
    pub size: u64,
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

/// What the matches that can be ticked add up to: the summary bar's total, kept current while
/// the view is closed too (counted again when the tree changes, once the scan is done);
/// `None` before the first count.
pub fn reclaimable(app: &App) -> Option<u64> {
    app.cleanup.reclaimable
}

/// The tree's version and its number of matches: the total is counted again when either
/// changes.
fn count_key(m: &Model) -> (u64, usize) {
    (m.tree.version(), m.tree.classes().count())
}

/// The view needs a read: it was just switched on.
pub fn stale(app: &App) -> bool {
    app.cleanup.on && !app.cleanup.fresh
}

/// The listed matches ([`dfdu_engine::listed`]), under the read lock. Matches are few: one per
/// matched directory, and below it only those a rule that does not stop the descent lets in.
pub fn read(m: &Model, app: &App) -> Info {
    if !app.cleanup.on {
        // Closed, only the total for the tab, when the tree changed since it was counted.
        let at = count_key(m);
        let Some(rules) = &m.rules else {
            return Info::default();
        };
        if app.snap.busy || app.cleanup.counted_at == Some(at) {
            return Info::default();
        }
        let total = (dfdu_engine::listed(&m.tree, rules).iter())
            .filter(|l| l.class.safety < Safety::Protected)
            .map(|l| l.total.alloc)
            .sum();
        return Info {
            total: Some(Reclaim { at, size: total }),
            ..Info::default()
        };
    }
    let t = &m.tree;
    let item = |ix: Ix, path: HostPath, class: Class, tot: Totals, scanning: bool| Item {
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
                    item(r, t.host_path(r), l.class, t.total(r), scanning)
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
    let total = items
        .iter()
        .filter(|it| markable(it))
        .map(|it| it.size)
        .sum();
    Info {
        items,
        native,
        native_sizes,
        total: Some(Reclaim {
            at: count_key(m),
            size: total,
        }),
    }
}

/// Takes in a read. The first read after the user opened the view ([`show`]) marks the
/// recommended items, unless it found no matches yet: then the next opening tries again.
pub fn apply(app: &mut App) {
    app.cleanup.fresh = true;
    if let Some(Reclaim { at, size }) = app.extras.cleanup.total {
        app.cleanup.reclaimable = Some(size);
        app.cleanup.counted_at = Some(at);
    }
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

/// The status line after `n` suggested items of `size` in all were ticked.
fn marked_status(n: usize, size: u64) -> String {
    format!(
        "Ticked {n} suggested item{} ({}) · {} untick all",
        plural(n),
        human(size),
        crate::app::keys::first_key(crate::app::keys::Action::ClearMarks)
    )
}

/// Whether `it` is marked by default, and why; `None` without rules.
fn recommended(app: &App, it: &Item, now: i64) -> Option<Verdict> {
    let rules = app.snap.rules.as_ref()?;
    Some(verdict(
        rules.rule(it.class.rule),
        it.class,
        it.size,
        now.saturating_sub(it.mtime),
        it.scanning,
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

/// The rows the page keys move the view's cursor by.
const PAGE: usize = 10;

/// Keys the view takes before the action registry; `None` passes the key on.
pub fn key(app: &mut App, k: &KeyEvent) -> Option<Vec<Effect>> {
    if app.cleanup.pane.is_some() {
        return Some(pane::key(app, k));
    }
    // What the first opening ticked is said until the next key.
    app.status = None;
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let action = keys::action_for(k)?;
    if let Some(c) = keys::cursor_after(action, app.cleanup.cursor, last, PAGE) {
        app.cleanup.cursor = c;
        return Some(Vec::new());
    }
    let c = &mut app.cleanup;
    match action {
        Action::Open | Action::Enter => match rows.get(c.cursor) {
            Some(&Row::Item(i)) => show_in_files(app, i),
            Some(&row) => {
                let open = is_open(app, row);
                set_open(app, row, !open);
            }
            None => {}
        },
        Action::Parent => {
            // The nearest open section or category at or above the cursor; none without rows.
            let at = (0..=c.cursor.min(last)).rev().find_map(|i| {
                let row = *rows.get(i)?;
                (!matches!(row, Row::Item(_)) && is_open(app, row)).then_some((i, row))
            });
            if let Some((i, row)) = at {
                set_open(app, row, false);
                app.cleanup.cursor = i;
            }
        }
        Action::ToggleMark => toggle(app, rows.get(app.cleanup.cursor).copied()),
        _ => return None,
    }
    Some(Vec::new())
}

/// The nodes of the matches the view lists and of what ticking them ticks (a container's
/// rest), for `d` to act on its ticks only.
pub fn listed(app: &App) -> HashSet<NodeRef> {
    (app.extras.cleanup.items.iter())
        .flat_map(|it| std::iter::once(it.node).chain(it.rest.iter().map(|r| r.node)))
        .collect()
}

/// Shows item `i` in Files: closes the view, opens the item's directory and selects it there.
fn show_in_files(app: &mut App, i: usize) {
    let Some(path) = app.extras.cleanup.items.get(i).map(|it| it.path.clone()) else {
        return;
    };
    let root = app.snap.root_path.clone();
    app.cleanup.on = false;
    app.go_to(&path, &root);
}

/// `f` ([`Action::CleanupFilter`](crate::app::keys::Action)): shows only the safer levels,
/// one level fewer each time, then everything again.
pub fn cycle_filter(app: &mut App) {
    let c = &mut app.cleanup;
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

/// `A` ([`Action::TickSuggested`](crate::app::keys::Action)): marks every recommended item.
pub fn tick_suggested(app: &mut App) {
    if app.read_only {
        app.status = Some(crate::app::keys::READ_ONLY.into());
        return;
    }
    let (n, size) = mark_recommended(app);
    app.status = Some(if n == 0 {
        "every suggested item is ticked already".into()
    } else {
        marked_status(n, size)
    });
}

/// `N` ([`Action::CleanupCommand`](crate::app::keys::Action)): opens the selected item's
/// native command, with its dry run.
pub fn command(app: &mut App) -> Vec<Effect> {
    pane::open_pane(app)
}

fn toggle(app: &mut App, row: Option<Row>) {
    let picks: Vec<usize> = match row {
        Some(Row::Item(i)) => vec![i],
        Some(Row::Group { cat, safety }) => group_items(app, cat, safety),
        Some(Row::Section(safety)) => section_items(app, safety),
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
pub(crate) mod tests;
