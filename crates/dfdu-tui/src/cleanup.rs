//! The cleanup view (`3`): every match of the rules, grouped by category
//! and safety, with the reclaimable total of each level. A group opens to its items (size,
//! entries, age, path); the selected item's explanation and what deleting it loses are shown
//! below. Marks are the list's: `d` and `D` delete what is marked through the same dialog.
//! The first time the view has items it marks the recommended ones: `safe` items above their
//! rule's minimum size, `regenerable` ones only when stale, never `review` or worse, and
//! nothing changed in the last 7 days or in use. `n` opens an item's native command: its dry
//! run first, then `y` runs it; `v` picks a further-reaching form, which `y` must confirm
//! twice.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use dfdu_engine::{Command, Model};
use dfdu_rules::Safety;
use dfdu_tree::{Class, DirState, Kind, NodeRef, RuleIx};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, Effect};
use crate::marks;
use crate::snapshot::{Ident, Target};
use crate::theme::{count, human};

/// Nothing changed this recently is recommended.
const QUIET_DAYS: i64 = 7;
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
    /// The recommended items were marked (once, when the view first had items).
    preselected: bool,
    pane: Option<Pane>,
    commands: u64,
    /// The extras were read since the view was switched on.
    fresh: bool,
}

/// The native command pane of one item.
#[derive(Clone, Debug)]
struct Pane {
    node: NodeRef,
    rule: RuleIx,
    variant: Option<usize>,
    /// The command whose output is shown; 0 before anything ran (no dry run to show).
    id: u64,
    dry_run: bool,
    /// A variant's real run was asked for once; asking again runs it.
    armed: bool,
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
/// numbers.
pub fn show(app: &mut App, on: bool) -> Vec<Effect> {
    if on && app.snap.rules.is_none() {
        app.status = Some("no rules: the cleanup view needs classification".into());
        return Vec::new();
    }
    let was = app.cleanup.on;
    app.cleanup.on = on;
    app.cleanup.fresh = false;
    if on && !was {
        crate::treemap::show(app, false);
        return vec![Effect::Send(Command::NativeSizes)];
    }
    Vec::new()
}

/// The view needs a read: it was just switched on.
pub fn stale(app: &App) -> bool {
    app.cleanup.on && !app.cleanup.fresh
}

/// Every match in the tree that is not inside another, under the read lock. Matches are few:
/// one per matched directory, nothing below it.
pub fn read(m: &Model, app: &App) -> Info {
    if !app.cleanup.on {
        return Info::default();
    }
    let t = &m.tree;
    let mut items: Vec<Item> = t
        .classes()
        .filter(|&(ix, _)| t.is_visible(ix))
        .filter(|&(ix, _)| t.parent(ix).is_none_or(|p| t.class_in_effect(p).is_none()))
        .map(|(ix, class)| {
            let tot = t.total(ix);
            Item {
                node: t.node_ref(ix),
                path: t.path_buf(ix),
                dev_ino: t.dev_ino(ix),
                kind: t.kind(ix),
                class,
                size: tot.alloc,
                items: tot.items,
                shared_out: tot.shared_out,
                mtime: t.mtime(ix),
                scanning: t.dir(ix).is_some_and(|d| d.state == DirState::Pending),
            }
        })
        .collect();
    items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
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

/// Takes in a read: marks the recommended items the first time there are any.
pub fn apply(app: &mut App) {
    app.cleanup.fresh = true;
    if !app.cleanup.on || app.cleanup.preselected || app.extras.cleanup.items.is_empty() {
        return;
    }
    app.cleanup.preselected = true;
    let n = mark_recommended(app);
    if n > 0 {
        app.status = Some(format!("marked {n} recommended item{}", plural(n)));
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Whether `it` is marked by default.
fn recommended(app: &App, it: &Item, now: i64) -> bool {
    let Some(rules) = &app.snap.rules else {
        return false;
    };
    recommends(
        rules.rule(it.class.rule),
        it.class,
        it.size,
        it.mtime,
        it.scanning,
        now,
    )
}

/// Whether a match of `rule` is recommended for deletion: `safe` above the rule's minimum
/// size, `regenerable` only when also unchanged for the rule's minimum age, never `review` or
/// worse, nothing changed in the last 7 days, in use, tracked by version control or still
/// being scanned. `size` is its disk usage, `mtime` and `now` in seconds since the epoch.
pub fn recommends(
    rule: &dfdu_rules::Rule,
    class: Class,
    size: u64,
    mtime: i64,
    scanning: bool,
    now: i64,
) -> bool {
    let days = |d: i64| now - mtime >= d * 86_400;
    let c = &class;
    let flagged = c.has(Class::IN_USE) || c.has(Class::VCS_TRACKED) || c.has(Class::VCS_UNKNOWN);
    if flagged || scanning || !days(QUIET_DAYS) {
        return false;
    }
    match c.safety {
        Safety::Safe => size >= rule.min_size,
        Safety::Regenerable => {
            size >= rule.min_size && days((rule.min_age_days as i64).max(QUIET_DAYS))
        }
        _ => false,
    }
}

/// Marks every recommended item; returns how many were newly marked.
fn mark_recommended(app: &mut App) -> usize {
    let now = now_secs();
    let picks: Vec<Item> = app
        .extras
        .cleanup
        .items
        .iter()
        .filter(|it| recommended(app, it, now) && !app.marks.contains(it.node))
        .cloned()
        .collect();
    for it in &picks {
        marks::set(app, mark_of(it), true);
    }
    picks.len()
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
        return Some(pane_key(app, k));
    }
    let rows = rows(app);
    let last = rows.len().saturating_sub(1);
    let c = &mut app.cleanup;
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
            Some(Row::Item(_)) => return Some(open_pane(app)),
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
        KeyCode::Char('a') => {
            let n = mark_recommended(app);
            app.status = Some(if n == 0 {
                "every recommended item is marked already".into()
            } else {
                format!("marked {n} recommended item{}", plural(n))
            });
        }
        KeyCode::Char('n') => return Some(open_pane(app)),
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
    let on = !items.iter().all(|it| app.marks.contains(it.node));
    for it in &items {
        marks::set(app, mark_of(it), on);
    }
    if matches!(row, Some(Row::Item(_))) {
        app.cleanup.cursor += 1;
    }
}

/// Opens the selected item's native command, starting with its dry run if it has one.
fn open_pane(app: &mut App) -> Vec<Effect> {
    let Some(it) = selected_item(app).cloned() else {
        return Vec::new();
    };
    let Some(rules) = app.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules
        .rule(it.class.rule)
        .native
        .as_ref()
        .filter(|d| !d.command.is_empty())
    else {
        app.status = Some("no native command for this item; d or D deletes it".into());
        return Vec::new();
    };
    if def.dry_run.is_empty() {
        app.cleanup.pane = Some(Pane {
            node: it.node,
            rule: it.class.rule,
            variant: None,
            id: 0,
            dry_run: true,
            armed: false,
        });
        return Vec::new();
    }
    start(app, &it, None, true)
}

fn start(app: &mut App, it: &Item, variant: Option<usize>, dry_run: bool) -> Vec<Effect> {
    app.cleanup.commands += 1;
    // Ids from the view are above any the dialog uses.
    let id = (1 << 32) + app.cleanup.commands;
    app.cleanup.pane = Some(Pane {
        node: it.node,
        rule: it.class.rule,
        variant,
        id,
        dry_run,
        armed: false,
    });
    vec![Effect::Send(Command::RunNative {
        id,
        node: it.node,
        variant,
        dry_run,
    })]
}

fn pane_key(app: &mut App, k: &KeyEvent) -> Vec<Effect> {
    let Some(p) = app.cleanup.pane.clone() else {
        return Vec::new();
    };
    let Some(rules) = app.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules.rule(p.rule).native.as_ref() else {
        return Vec::new();
    };
    // Until its result has arrived, the shown command counts as running.
    let done = app
        .extras
        .cleanup
        .native
        .as_ref()
        .filter(|n| n.id == p.id)
        .and_then(|n| n.done.clone());
    let running = p.id != 0 && done.is_none();
    // The real run only after nothing ran yet (no dry run to show) or a dry run that worked.
    let may_run = p.id == 0 || (p.dry_run && done == Some(Ok(())));
    let it = app
        .extras
        .cleanup
        .items
        .iter()
        .find(|i| i.node == p.node)
        .cloned();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') if !running => app.cleanup.pane = None,
        KeyCode::Char('v') if !running => {
            let next = match p.variant {
                None if !def.variants.is_empty() => Some(0),
                Some(v) if v + 1 < def.variants.len() => Some(v + 1),
                _ => None,
            };
            let dry = match next {
                None => !def.dry_run.is_empty(),
                Some(v) => !def.variants[v].dry_run.is_empty(),
            };
            if let Some(it) = it {
                if dry {
                    return start(app, &it, next, true);
                }
                app.cleanup.pane = Some(Pane {
                    variant: next,
                    id: 0,
                    dry_run: true,
                    armed: false,
                    ..p
                });
            }
        }
        KeyCode::Char('y') if may_run => {
            let Some(it) = it else {
                return Vec::new();
            };
            if p.variant.is_some() && !p.armed {
                if let Some(pane) = app.cleanup.pane.as_mut() {
                    pane.armed = true;
                }
                app.status = Some("press y again to run it".into());
                return Vec::new();
            }
            return start(app, &it, p.variant, false);
        }
        _ => {}
    }
    Vec::new()
}

/// Draws the view in `area`.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.snap.rules.is_none() {
        return;
    }
    let rows = rows(app);
    let mut lines = summary_lines(app);
    // The rule text below gives way to the list on short terminals: three rows stay for it.
    let detail_h: u16 = 4.min(area.height.saturating_sub(lines.len() as u16 + 1 + 3));
    let list_h = area
        .height
        .saturating_sub(lines.len() as u16 + detail_h + 1) as usize;
    app.cleanup.clamp_scroll(rows.len(), list_h);
    let app: &App = app;
    let t = &app.theme;
    let (cursor, scroll) = (app.cleanup.cursor, app.cleanup.scroll);
    if rows.is_empty() {
        lines.push(Line::styled(
            if app.snap.busy {
                " nothing matched yet; still scanning"
            } else {
                " nothing the rules know about was found"
            },
            t.fg_dim,
        ));
    }
    let now = now_secs();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(list_h) {
        let mut spans = match *row {
            Row::Group { cat, safety } => group_line(app, cat, safety),
            Row::Item(i) => item_line(app, &app.extras.cleanup.items[i], now),
        };
        if i == cursor {
            for s in &mut spans {
                s.style = s.style.patch(t.cursor);
            }
        }
        lines.push(Line::from(spans));
    }
    let [top, bottom] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(detail_h),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(lines), top);
    let detail = detail_lines(app, rows.get(cursor));
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), bottom);
    draw_pane(f, app, area, t);
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

/// The reclaimable total of each safety level, and what native commands count themselves.
fn summary_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut by = [0u64; Safety::ALL.len()];
    for it in &app.extras.cleanup.items {
        by[it.class.safety as usize] += it.size.saturating_sub(it.shared_out);
    }
    let mut sum = vec![Span::raw(" reclaimable  ")];
    for s in [
        Safety::Safe,
        Safety::Regenerable,
        Safety::Review,
        Safety::Manual,
    ] {
        sum.push(Span::styled(format!("{} ", s.as_str()), t.fg_dim));
        sum.push(Span::raw(format!("{}   ", human(by[s as usize]))));
    }
    let mut lines = vec![Line::from(sum)];
    let native = &app.extras.cleanup.native_sizes;
    if !native.is_empty() {
        let parts: Vec<String> = native
            .iter()
            .map(|(name, r)| match r {
                Ok(b) => format!("{name}: {} by its own count", human(*b)),
                Err(e) => format!("{name}: {e}"),
            })
            .collect();
        lines.push(Line::styled(format!(" {}", parts.join(" · ")), t.fg_dim));
    }
    lines
}

/// The row of the group of category `cat` at `safety`: its size, items and how many are marked.
fn group_line(app: &App, cat: u16, safety: Safety) -> Vec<Span<'static>> {
    let t = &app.theme;
    let items = &app.extras.cleanup.items;
    let ix = group_items(app, cat, safety);
    let size: u64 = ix.iter().map(|&i| items[i].size).sum();
    let marked = ix
        .iter()
        .filter(|&&i| app.marks.contains(items[i].node))
        .count();
    let category = &dfdu_rules::CATEGORIES[cat as usize];
    let open = app.cleanup.open.contains(&(cat, safety));
    vec![
        Span::raw(if open { " ▾ " } else { " ▸ " }),
        Span::styled(
            format!("{:<24}", category.name),
            t.cat[category.color as usize],
        ),
        Span::raw(format!(" {:<12}", safety.as_str())),
        Span::raw(format!("{:>10}", human(size))),
        Span::styled(
            format!(
                "  {} item{}{}",
                ix.len(),
                plural(ix.len()),
                if marked > 0 {
                    format!(", {marked} marked")
                } else {
                    String::new()
                }
            ),
            t.fg_dim,
        ),
    ]
}

/// The row of `it`: its mark, size, items, age in days at `now`, path and warnings.
fn item_line(app: &App, it: &Item, now: i64) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mark = if app.marks.contains(it.node) {
        Span::styled("  ● ", t.mark)
    } else if markable(it) {
        Span::raw("    ")
    } else {
        Span::styled("  ⊘ ", t.fg_dim)
    };
    let age = (now - it.mtime).max(0) / 86_400;
    let mut v = vec![
        mark,
        Span::raw(format!("{:>10}", human(it.size))),
        Span::styled(format!("  {:>6}", count(it.items)), t.fg_dim),
        Span::styled(format!("  {:>4} d", age), t.fg_dim),
        Span::raw(format!("  {}", shown_path(&it.path))),
    ];
    for (flag, what) in [
        (Class::IN_USE, "in use"),
        (Class::VCS_TRACKED, "git-tracked"),
        (Class::VCS_UNKNOWN, "git unknown"),
    ] {
        if it.class.has(flag) {
            v.push(Span::styled(format!("  {what}"), t.status_warn));
        }
    }
    v
}

/// The selected item's rule: what it is and what deleting it loses; the keys on a group.
fn detail_lines<'a>(app: &'a App, row: Option<&Row>) -> Vec<Line<'a>> {
    let t = &app.theme;
    let (Some(Row::Item(i)), Some(rules)) = (row, &app.snap.rules) else {
        return vec![Line::styled(
            " Enter opens a group · Space marks · a marks the recommended · f filters by safety",
            t.fg_dim,
        )];
    };
    let it = &app.extras.cleanup.items[*i];
    let r = rules.rule(it.class.rule);
    let mut d = vec![
        Line::from(vec![
            Span::styled(format!(" {}", r.name), t.title),
            Span::styled(format!("  {}", it.class.safety.as_str()), t.fg_dim),
        ]),
        Line::raw(format!(" {}", r.explanation)),
        Line::styled(format!(" Lost: {}", r.lost), t.fg_dim),
    ];
    if r.native.as_ref().is_some_and(|n| !n.command.is_empty()) {
        d.push(Line::styled(" n runs its own cleanup command", t.fg_dim));
    }
    d
}

fn shown_path(p: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rel) => format!("~/{}", rel.display()),
        None => p.display().to_string(),
    }
}

fn draw_pane(f: &mut Frame, app: &App, area: Rect, t: &crate::theme::Theme) {
    let Some(p) = &app.cleanup.pane else {
        return;
    };
    let Some(rules) = &app.snap.rules else {
        return;
    };
    let r = rules.rule(p.rule);
    let Some(def) = r.native.as_ref() else {
        return;
    };
    let w = area.width.saturating_sub(4).max(20);
    let h = area.height.saturating_sub(2).max(6);
    let rect = Rect::new(
        area.x + 2,
        area.y + 1,
        w.min(area.width),
        h.min(area.height),
    );
    f.render_widget(Clear, rect);
    let shown = app.extras.cleanup.native.as_ref().filter(|n| n.id == p.id);
    let what = match p.variant {
        None => "its cleanup command".to_string(),
        Some(v) => def
            .variants
            .get(v)
            .map_or(String::new(), |x| x.name.clone()),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {}: ", r.name), t.title),
        Span::raw(what),
    ])];
    let body = rect.height.saturating_sub(5) as usize;
    if let Some(n) = shown {
        lines.extend(
            n.lines
                .iter()
                .skip(n.lines.len().saturating_sub(body))
                .map(|l| Line::raw(format!(" {l}"))),
        );
    }
    lines.push(Line::raw(""));
    let command = match p.variant {
        None => &def.command,
        Some(v) => def.variants.get(v).map_or(&def.command, |x| &x.command),
    };
    let state = match shown.and_then(|n| n.done.as_ref()) {
        None if p.id == 0 => Line::from(vec![
            Span::raw(format!(" Runs {}. ", dfdu_engine::shown_argv(command))),
            Span::styled("y", t.key),
            Span::raw(" run it  "),
            Span::styled("v", t.key),
            Span::raw(" other form  "),
            Span::styled("Esc", t.key),
            Span::raw(" close"),
        ]),
        None => Line::styled(" running…", t.fg_dim),
        Some(Ok(())) if p.dry_run => Line::from(vec![
            Span::raw(" That was the dry run. "),
            Span::styled("y", t.key),
            Span::raw(" run it  "),
            Span::styled("v", t.key),
            Span::raw(" other form  "),
            Span::styled("Esc", t.key),
            Span::raw(" close"),
        ]),
        Some(Ok(())) => Line::styled(" Done; the directory is rescanned.", t.status_ok),
        Some(Err(e)) => Line::styled(format!(" Failed: {e}"), t.status_error),
    };
    lines.push(state);
    if p.armed {
        lines.push(Line::styled(
            " This form removes more than the default: press y again to run it.",
            t.status_warn,
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().border_style(t.border_focus))
            .style(Style::new()),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::tests::{press, render};
    use crate::theme::Theme;
    use crate::treemap::tests::{classified, sync};

    #[test]
    fn groups_by_category_and_safety_and_marks_the_recommended() {
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        let fx_out = app.key(press(KeyCode::Char('3')), t0);
        assert!(
            matches!(fx_out.as_slice(), [Effect::Send(Command::NativeSizes)]),
            "{fx_out:?}"
        );
        sync(&mut app, &fx.m, t0);
        assert!(active(&app));
        let items = &app.extras.cleanup.items;
        assert_eq!(items.len(), 2, "{items:?}");
        // Both are below their rules' minimum sizes: nothing is recommended.
        assert!(app.marks.is_empty());
        let frame = render(&mut app, t0, 100, 24);
        assert!(frame.contains("reclaimable"), "{frame}");
        assert!(frame.contains("▸ OS and user caches"), "{frame}");
        insta::assert_snapshot!("cleanup_100x24", frame);

        // The first group is the safe one; Enter opens it, Space marks its item and again
        // unmarks it.
        app.key(press(KeyCode::Enter), t0);
        let frame = render(&mut app, t0, 100, 24);
        assert!(frame.contains("▾ OS and user caches"), "{frame}");
        app.key(press(KeyCode::Down), t0);
        app.key(press(KeyCode::Char(' ')), t0);
        assert!(app.marks.items()[0].target.path.ends_with("mid/m0"));
        app.key(press(KeyCode::Up), t0);
        app.key(press(KeyCode::Char(' ')), t0);
        assert!(app.marks.is_empty(), "Space on a group toggles all of it");
        // `f` shows safe items only: the regenerable group goes.
        app.key(press(KeyCode::Char('f')), t0);
        let frame = render(&mut app, t0, 100, 24);
        assert!(!frame.contains("Build output"), "{frame}");
        // Back to the list.
        app.key(press(KeyCode::Char('1')), t0);
        assert!(!active(&app));
    }

    /// The details pane describes the item selected in the cleanup view, not the list's entry.
    #[test]
    fn details_follow_the_cleanup_selection() {
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        app.key(press(KeyCode::Char('3')), t0);
        sync(&mut app, &fx.m, t0);
        app.key(press(KeyCode::Enter), t0);
        app.key(press(KeyCode::Down), t0);
        app.key(press(KeyCode::Char('i')), t0);
        sync(&mut app, &fx.m, t0);
        let want = selected_item(&app).unwrap().path.clone();
        let facts = app.extras.details.facts.as_ref().expect("details read");
        assert_eq!(facts.path, want);
        // On a group row there is nothing to describe.
        app.key(press(KeyCode::Up), t0);
        sync(&mut app, &fx.m, t0);
        assert!(app.extras.details.facts.is_none());
    }

    /// On a short terminal the rule text gives way so that the list still shows rows.
    #[test]
    fn a_short_terminal_still_shows_the_groups() {
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        app.key(press(KeyCode::Char('3')), t0);
        sync(&mut app, &fx.m, t0);
        let frame = render(&mut app, t0, 100, 10);
        assert!(frame.contains("▸ OS and user caches"), "{frame}");
    }

    #[test]
    fn recommended_items() {
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        app.key(press(KeyCode::Char('3')), t0);
        sync(&mut app, &fx.m, t0);
        let rules = app.snap.rules.clone().unwrap();
        let base = app.extras.cleanup.items[0].clone();
        let rule = |id: &str| rules.index(id).unwrap();
        let now = now_secs();
        let day = 86_400;
        let it = |id: &str, safety: Safety, size: u64, age_days: i64, flags: u8| Item {
            class: Class {
                rule: rule(id),
                safety,
                flags,
                ..base.class
            },
            size,
            mtime: now - age_days * day,
            ..base.clone()
        };
        let gib = 1 << 30;
        // Safe above its minimum (50 MiB), quiet for a week.
        assert!(recommended(
            &app,
            &it("generic.cachedir-tag", Safety::Safe, gib, 8, 0),
            now
        ));
        assert!(!recommended(
            &app,
            &it("generic.cachedir-tag", Safety::Safe, 1 << 20, 8, 0),
            now
        ));
        assert!(!recommended(
            &app,
            &it("generic.cachedir-tag", Safety::Safe, gib, 2, 0),
            now
        ));
        // Regenerable only when stale (the Rust rule: 14 days).
        assert!(recommended(
            &app,
            &it("rust.target", Safety::Regenerable, gib, 15, 0),
            now
        ));
        assert!(!recommended(
            &app,
            &it("rust.target", Safety::Regenerable, gib, 10, 0),
            now
        ));
        // Never review, nothing in use or tracked by git.
        assert!(!recommended(
            &app,
            &it("rust.target", Safety::Review, gib, 99, 0),
            now
        ));
        let in_use = Class::IN_USE;
        assert!(!recommended(
            &app,
            &it("rust.target", Safety::Regenerable, gib, 99, in_use),
            now
        ));
    }

    #[test]
    fn native_commands_start_with_their_dry_run() {
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
        app.key(press(KeyCode::Char('3')), t0);
        sync(&mut app, &fx.m, t0);
        // The regenerable group holds `big`, a Rust target/ cleaned with `cargo clean`.
        let groups = rows(&app);
        let at = groups
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Row::Group {
                        safety: Safety::Regenerable,
                        ..
                    }
                )
            })
            .unwrap();
        app.cleanup.cursor = at;
        app.key(press(KeyCode::Enter), t0);
        app.key(press(KeyCode::Down), t0);
        let out = app.key(press(KeyCode::Char('n')), t0);
        assert!(
            matches!(
                out.as_slice(),
                [Effect::Send(Command::RunNative {
                    dry_run: true,
                    variant: None,
                    ..
                })]
            ),
            "{out:?}"
        );
        // While it runs, `y` does nothing.
        assert!(app.key(press(KeyCode::Char('y')), t0).is_empty());
        let id = app.cleanup.pane.as_ref().unwrap().id;
        app.extras.cleanup.native = Some(NativeView {
            id,
            command: "cargo clean --dry-run".into(),
            lines: vec!["Summary 3 files".into()],
            done: Some(Ok(())),
        });
        let frame = render(&mut app, t0, 100, 24);
        assert!(frame.contains("That was the dry run"), "{frame}");
        let out = app.key(press(KeyCode::Char('y')), t0);
        assert!(
            matches!(
                out.as_slice(),
                [Effect::Send(Command::RunNative { dry_run: false, .. })]
            ),
            "{out:?}"
        );
    }
}
