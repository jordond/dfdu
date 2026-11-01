//! The cleanup view: every match of the rules in the whole scan. Sections by safety level (the
//! decision being made) hold the categories found at that level, which open to their items
//! ([`rows`]); levels without matches are left out. Ticks are the marks.
//!
//! The first time the user opens the view and it has items, it ticks the suggested ones:
//! `safe` items above their rule's minimum size, `regenerable` ones only when stale, never
//! `review` or worse, and nothing changed in the last 7 days or in use ([`recommended`]). A
//! read-only browser marks nothing, as nothing marked could be deleted. What a front end's keys
//! ask of the view arrives as an [`Intent`].

use std::collections::HashSet;

use dfdu_engine::{Command, Model, Verdict, verdict};
use dfdu_rules::CategoryIx;
use dfdu_tree::{
    Class, DirState, HostPath, Ident, Ix, Kind, NodeRef, Safety, Target, Totals, now_secs,
};

use crate::actions::{Action, first_key};
use crate::effect::Effect;
use crate::format::{human, plural};
use crate::marks;
use crate::state::{READ_ONLY, SessionState};
use crate::view::{Then, View};

mod rows;

pub use rows::{Row, enter_does, group_items, is_open, rows, section_items, selected, set_open};

/// Output lines of a native command kept.
const PANE_LINES: usize = 200;

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Cleanup {
    /// The selected row.
    pub cursor: usize,
    /// Categories shown with their items, by (category, safety); closed by default.
    pub open: HashSet<(CategoryIx, Safety)>,
    /// Safety sections closed by the user; open by default.
    pub closed: HashSet<Safety>,
    /// Only items this safe or safer; `None` shows all.
    pub filter: Option<Safety>,
    /// The recommended items were marked (once, when the view was first opened with items).
    preselected: bool,
    /// The view was just opened: the next read marks the recommended items if it finds any
    /// matches, and they were never marked before.
    premark: bool,
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

/// The output of a native command the view shows.
#[derive(Clone, Debug, Default)]
pub struct NativeView {
    pub id: u64,
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
    /// The tree's version and number of matches it was counted at.
    pub at: (u64, usize),
    pub size: u64,
}

/// What a front end's keys ask of the view.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Puts the cursor on row `n` of [`rows`].
    Select(usize),
    /// Opens or closes the selected section or category, or shows the selected item in Files.
    Activate,
    /// Closes the nearest open section or category at or above the cursor, and selects it.
    CloseUp,
    /// Ticks or unticks the selected row: an item, every item of a category, or of a level.
    Toggle,
    /// Shows only the safer levels, one level fewer each time, then everything again.
    CycleFilter,
    /// Marks every recommended item.
    TickSuggested,
}

/// True when the view is shown, and so takes the intents it knows.
pub fn active(s: &SessionState) -> bool {
    s.current_view() == View::Cleanup
}

/// The view was opened, or opened `again` while shown: it reads its matches again, and until
/// it once did, marks the recommended items ([`apply`]). Opened from another view, it also asks
/// the rules' native size commands for their numbers.
pub fn on_open(s: &mut SessionState, again: bool) -> Vec<Effect> {
    s.cleanup.fresh = false;
    s.cleanup.premark = !s.cleanup.preselected && !s.read_only;
    if again {
        return Vec::new();
    }
    vec![Effect::Send(Command::NativeSizes)]
}

/// What the matches that can be ticked add up to: the summary bar's total, kept current while
/// the view is closed too (counted again when the tree changes, once the scan is done);
/// `None` before the first count.
pub fn reclaimable(s: &SessionState) -> Option<u64> {
    s.cleanup.reclaimable
}

/// The tree's version and its number of matches: the total is counted again when either
/// changes.
fn count_key(m: &Model) -> (u64, usize) {
    (m.tree.version(), m.tree.classes().count())
}

/// The view needs a read: it was just switched on.
pub fn stale(s: &SessionState) -> bool {
    active(s) && !s.cleanup.fresh
}

/// The listed matches ([`dfdu_engine::listed`]), under the read lock. Matches are few: one per
/// matched directory, and below it only those a rule that does not stop the descent lets in.
pub fn read(m: &Model, s: &SessionState) -> Info {
    // Without the rules there is nothing to list or count, and the view is not shown
    // ([`SessionState::current_view`]).
    let Some(rules) = &m.rules else {
        return Info::default();
    };
    if !active(s) {
        // Closed, only the total for the tab, when the tree changed since it was counted.
        let at = count_key(m);
        if s.snap.busy || s.cleanup.counted_at == Some(at) {
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
    let items: Vec<Item> = dfdu_engine::listed(t, rules)
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
        lines: n
            .lines
            .iter()
            .skip(n.lines.len().saturating_sub(PANE_LINES))
            .cloned()
            .collect(),
        done: n.done.clone(),
    });
    let mut native_sizes: Vec<(String, Result<u64, String>)> = (m.native_sizes.iter())
        .map(|(&r, v)| (rules.rule(r).name.clone(), v.clone()))
        .collect();
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

/// Takes in a read, now in `s.extras.cleanup`. The first read after the user opened the view
/// ([`on_open`]) marks the recommended items, unless it found no matches yet: then the next
/// opening tries again.
pub fn apply(s: &mut SessionState) {
    s.cleanup.fresh = true;
    if let Some(Reclaim { at, size }) = s.extras.cleanup.total {
        s.cleanup.reclaimable = Some(size);
        s.cleanup.counted_at = Some(at);
    }
    let premark = std::mem::take(&mut s.cleanup.premark);
    if !active(s) || !premark || s.extras.cleanup.items.is_empty() {
        return;
    }
    s.cleanup.preselected = true;
    let (n, size) = mark_recommended(s);
    if n > 0 {
        s.status = Some(marked_status(n, size));
    }
}

/// The status line after `n` suggested items of `size` in all were ticked.
fn marked_status(n: usize, size: u64) -> String {
    format!(
        "Ticked {n} suggested item{} ({}) · {} untick all",
        plural(n as u64),
        human(size),
        first_key(Action::ClearMarks)
    )
}

/// Whether `it` is marked by default, and why, `now` (seconds since the Unix epoch); `None`
/// without rules.
pub fn recommended(s: &SessionState, it: &Item, now: i64) -> Option<Verdict> {
    let rules = s.snap.rules.as_ref()?;
    Some(verdict(
        rules.rule(it.class.rule),
        it.class,
        it.size,
        now.saturating_sub(it.mtime),
        it.scanning,
    ))
}

/// Marks every recommended item; returns how many were newly marked, and their size.
fn mark_recommended(s: &mut SessionState) -> (usize, u64) {
    let now = now_secs();
    let picks: Vec<Item> = (s.extras.cleanup.items.iter())
        .filter(|it| recommended(s, it, now).is_some_and(Verdict::recommended))
        .filter(|it| !is_marked(s, it))
        .cloned()
        .collect();
    for it in &picks {
        set_marked(s, it, true);
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
pub fn is_marked(s: &SessionState, it: &Item) -> bool {
    if it.rest.is_empty() {
        s.marks.contains(it.node)
    } else {
        it.rest.iter().all(|r| s.marks.contains(r.node))
    }
}

/// Marks or unmarks `it`, or each entry of its rest.
fn set_marked(s: &mut SessionState, it: &Item, on: bool) {
    if it.rest.is_empty() {
        marks::set(s, mark_of(it), on);
    }
    for r in &it.rest {
        marks::set(s, mark_of(r), on);
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
pub fn markable(it: &Item) -> bool {
    it.class.safety < Safety::Protected
}

/// The nodes of the matches the view lists and of what ticking them ticks (a container's
/// rest), for a delete to act on its ticks only.
pub fn listed(s: &SessionState) -> HashSet<NodeRef> {
    (s.extras.cleanup.items.iter())
        .flat_map(|it| std::iter::once(it.node).chain(it.rest.iter().map(|r| r.node)))
        .collect()
}

/// Carries out `intent`.
pub fn act(s: &mut SessionState, intent: Intent) -> Then {
    let rows = rows(s);
    let last = rows.len().saturating_sub(1);
    match intent {
        Intent::Select(row) => s.cleanup.cursor = row,
        Intent::Activate => match rows.get(s.cleanup.cursor) {
            Some(&Row::Item(i)) => return show_in_files(s, i),
            Some(&row) => {
                let open = is_open(s, row);
                set_open(s, row, !open);
            }
            None => {}
        },
        Intent::CloseUp => {
            // The nearest open section or category at or above the cursor; none without rows.
            let at = (0..=s.cleanup.cursor.min(last)).rev().find_map(|i| {
                let row = *rows.get(i)?;
                (!matches!(row, Row::Item(_)) && is_open(s, row)).then_some((i, row))
            });
            if let Some((i, row)) = at {
                set_open(s, row, false);
                s.cleanup.cursor = i;
            }
        }
        Intent::Toggle => toggle(s, rows.get(s.cleanup.cursor).copied()),
        Intent::CycleFilter => cycle_filter(s),
        Intent::TickSuggested => tick_suggested(s),
    }
    Then::nothing()
}

/// Shows item `i` in Files, in its directory.
fn show_in_files(s: &SessionState, i: usize) -> Then {
    match s.extras.cleanup.items.get(i) {
        Some(it) => Then::show(it.path.clone(), s.snap.root_path.clone()),
        None => Then::nothing(),
    }
}

/// Shows only the safer levels, one level fewer each time, then everything again.
fn cycle_filter(s: &mut SessionState) {
    let c = &mut s.cleanup;
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
    s.status = Some(format!("cleanup: showing {label}"));
}

/// Marks every recommended item.
fn tick_suggested(s: &mut SessionState) {
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return;
    }
    let (n, size) = mark_recommended(s);
    s.status = Some(if n == 0 {
        "every suggested item is ticked already".into()
    } else {
        marked_status(n, size)
    });
}

fn toggle(s: &mut SessionState, row: Option<Row>) {
    let picks: Vec<usize> = match row {
        Some(Row::Item(i)) => vec![i],
        Some(Row::Group { cat, safety }) => group_items(s, cat, safety),
        Some(Row::Section(safety)) => section_items(s, safety),
        None => return,
    };
    let items: Vec<Item> = (picks.iter())
        .filter_map(|&i| s.extras.cleanup.items.get(i).cloned())
        .filter(markable)
        .collect();
    if items.is_empty() {
        s.status = Some("protected: cannot be marked".into());
        return;
    }
    let on = !items.iter().all(|it| is_marked(s, it));
    for it in &items {
        set_marked(s, it, on);
    }
    if matches!(row, Some(Row::Item(_))) {
        s.cleanup.cursor += 1;
    }
}

#[cfg(test)]
mod tests;
