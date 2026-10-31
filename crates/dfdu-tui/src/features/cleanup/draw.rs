//! Drawing the view in the review layout (`features::review`): the summary bar, what native
//! commands count themselves, the column names, then the sections by safety, their categories
//! and items, with the selected row's rule in the detail pane; the native command pane goes over
//! them.

use dfdu_engine::Verdict;
use dfdu_rules::CategoryIx;
use dfdu_tree::{ClassFlags, Safety, now_secs};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{
    Item, Row, group_items, is_marked, is_open, markable, pane, recommended, rows, section_items,
};
use crate::app::App;
use crate::app::keys::{Action, first_key};
use crate::features::review::{self, CHECK_WIDTH, Tally, Tick};
use crate::format::{count, human};
use crate::model::outside::tilde;
use crate::view::cells;
use crate::view::columns::cursor;
use crate::view::empty::{self, Empty};

/// Before an item's checkbox, under its category's name.
const ITEM_INDENT: usize = 7;
/// Cells of the size, entries and age columns of an item.
const SIZE: usize = 9;
const ITEMS: usize = 6;
const AGE: usize = 8;
/// The reason chips are at most this wide.
const CHIP: usize = 11;

/// A safety level as a section: its name and what it means for the user.
fn section_name(s: Safety) -> (&'static str, &'static str) {
    match s {
        Safety::Safe => ("Safe to delete", "caches the owning program recreates"),
        Safety::Regenerable => ("Regenerable", "rebuilt or reinstalled when needed"),
        Safety::Review => ("Review first", "may hold work"),
        Safety::Manual => ("Manual", "remove with the owner's own tool"),
        Safety::Protected => ("Protected", "never deleted"),
    }
}

/// Draws the view in `area`.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.snap.rules.is_none() {
        return;
    }
    let rows = rows(app);
    if rows.is_empty() {
        let e = Empty {
            title: "Cleanup",
            text: vec![
                "Lists what the rules recognise in the whole scan (caches, build output, \
                 dependencies), by how safe it is to delete."
                    .into(),
                if app.snap.busy {
                    "Nothing matched yet; the scan is still running.".into()
                } else {
                    "Nothing the rules know about was found.".into()
                },
            ],
            keys: vec![(first_key(Action::Close), "back to Files")],
        };
        empty::draw(f, &app.theme, area, &e);
        return;
    }
    let mut head = vec![review::summary(app, tally(app), "items", "", area.width)];
    head.extend(notes(app));
    let head_h = cells(head.len()).min(area.height);
    let body = Rect {
        y: area.y + head_h,
        height: area.height - head_h,
        ..area
    };
    let (list, pane_area) = review::split(body, 5);
    let list_h = usize::from(list.height).saturating_sub(1);
    app.cleanup.clamp_scroll(rows.len(), list_h);
    let app: &App = app;
    let t = &app.theme;
    let (cursor_at, scroll) = (app.cleanup.cursor, app.cleanup.scroll);
    f.render_widget(Paragraph::new(head), area);
    let headings = format!(
        "{:indent$}{:>SIZE$}  {:>ITEMS$}  {:>AGE$}  path",
        "",
        "size",
        "items",
        "modified",
        indent = ITEM_INDENT + CHECK_WIDTH
    );
    let mut lines = vec![Line::styled(headings, t.fg_dim)];
    let now = now_secs();
    let width = usize::from(list.width);
    let shared = review::shared_prefix(
        (rows.iter())
            .filter_map(|r| match r {
                Row::Item(i) => Some(tilde(
                    &app.extras.cleanup.items[*i].path,
                    app.home.as_deref(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str),
    );
    for (i, row) in rows.iter().enumerate().skip(scroll).take(list_h) {
        let line = match *row {
            Row::Section(safety) => section_line(app, safety, width),
            Row::Group { cat, safety } => group_line(app, cat, safety, width),
            Row::Item(i) => item_line(app, &app.extras.cleanup.items[i], now, shared, width),
        };
        lines.push(if i == cursor_at {
            cursor(t, line, list.width)
        } else {
            line
        });
    }
    f.render_widget(Paragraph::new(lines), list);
    if let Some(p) = pane_area {
        let detail = detail_lines(app, rows.get(cursor_at), now);
        review::draw_detail(f, t, p, review::on_right(body), detail);
    }
    pane::draw(f, app, area, t);
}

/// What the summary bar counts: the items that can be ticked, as the tab's total does
/// (`super::reclaimable`).
fn tally(app: &App) -> Tally {
    let mut sum = Tally::default();
    for it in app.extras.cleanup.items.iter().filter(|it| markable(it)) {
        sum.add(it.size, is_marked(app, it));
    }
    sum
}

/// Under the summary bar: what native commands count themselves, and rule locations behind
/// links out of the scan.
fn notes(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut lines = Vec::new();
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
    if let Some(rules) = &app.snap.rules {
        lines.extend(crate::model::outside::view_lines(app, rules));
    }
    lines
}

/// The tick of the items `ix`: on when every one that can be ticked is.
fn tick_of_items(app: &App, ix: &[usize]) -> Tick {
    let items = &app.extras.cleanup.items;
    let can: Vec<&Item> = ix
        .iter()
        .map(|&i| &items[i])
        .filter(|it| markable(it))
        .collect();
    if can.is_empty() {
        return Tick::Locked;
    }
    let ticked = can.iter().filter(|it| is_marked(app, it)).count();
    review::tick_of(ticked, can.len())
}

/// `left` and, right-aligned at the end of `width` cells, the size and count of `ix`.
fn with_total(
    app: &App,
    mut left: Vec<Span<'static>>,
    ix: &[usize],
    width: usize,
) -> Line<'static> {
    let size: u64 = ix.iter().map(|&i| app.extras.cleanup.items[i].size).sum();
    let right = format!("{:>SIZE$}  {:>4} ", human(size), ix.len());
    let used: usize = left.iter().map(Span::width).sum();
    let room = width.saturating_sub(right.chars().count());
    if used > room {
        // The meaning after the name gives way to the total.
        let mut cut = Vec::new();
        let mut w = 0;
        for s in left {
            if w + s.width() > room {
                break;
            }
            w += s.width();
            cut.push(s);
        }
        left = cut;
    }
    let used: usize = left.iter().map(Span::width).sum();
    left.push(Span::raw(" ".repeat(room.saturating_sub(used))));
    left.push(Span::styled(right, app.theme.fg_dim));
    Line::from(left)
}

/// A safety level's row: `▾ [-] Safe to delete · caches the owning program recreates`.
fn section_line(app: &App, safety: Safety, width: usize) -> Line<'static> {
    let t = &app.theme;
    let ix = section_items(app, safety);
    let (name, meaning) = section_name(safety);
    let arrow = if is_open(app, Row::Section(safety)) {
        " ▾ "
    } else {
        " ▸ "
    };
    let left = vec![
        Span::raw(arrow),
        review::checkbox(t, tick_of_items(app, &ix)),
        Span::styled(name, t.title),
        Span::styled(format!(" · {meaning}"), t.fg_dim),
    ];
    with_total(app, left, &ix, width)
}

/// A category's row in its section, in the category's colour.
fn group_line(app: &App, cat: CategoryIx, safety: Safety, width: usize) -> Line<'static> {
    let t = &app.theme;
    let ix = group_items(app, cat, safety);
    let category = cat.get();
    let arrow = if is_open(app, Row::Group { cat, safety }) {
        "   ▾ "
    } else {
        "   ▸ "
    };
    let left = vec![
        Span::raw(arrow),
        review::checkbox(t, tick_of_items(app, &ix)),
        Span::styled(category.name, t.cat[category.color as usize]),
    ];
    with_total(app, left, &ix, width)
}

/// Why an item is or is not suggested, as a one-word chip; the detail pane has the sentence.
fn chip(v: Verdict) -> &'static str {
    match v {
        Verdict::Safe | Verdict::Stale(_) => "✓ suggested",
        Verdict::Newer(_) => "recent",
        Verdict::Below(_) => "small",
        Verdict::InUse => "in use",
        Verdict::Scanning => "scanning",
        Verdict::Level(_) | Verdict::Tracked | Verdict::VcsUnknown => "review",
    }
}

/// The same as a sentence, for the detail pane.
fn reason(v: Verdict) -> String {
    match v {
        Verdict::Safe => "Suggested: safe to delete.".into(),
        Verdict::Stale(d) => format!("Suggested: unchanged for over {d} days."),
        Verdict::Level(s) => format!("Not suggested: {} items never are.", s.as_str()),
        Verdict::InUse => "Not suggested: in use.".into(),
        Verdict::Tracked => "Not suggested: tracked by git.".into(),
        Verdict::VcsUnknown => "Not suggested: git could not tell if it is tracked.".into(),
        Verdict::Scanning => "Not suggested until its scan finishes.".into(),
        Verdict::Newer(d) => format!("Not suggested: changed in the last {d} days."),
        Verdict::Below(min) => format!("Not suggested: below the rule's {}.", human(min)),
    }
}

/// The row of `it`, `width` cells wide: its checkbox, size, entries, age at `now`, path (the
/// first `shared` characters dimmed) and reason chip.
fn item_line(app: &App, it: &Item, now: i64, shared: usize, width: usize) -> Line<'static> {
    let t = &app.theme;
    let tick = if !markable(it) {
        Tick::Locked
    } else if is_marked(app, it) {
        Tick::On
    } else {
        Tick::Off
    };
    let mut spans = vec![
        Span::raw(" ".repeat(ITEM_INDENT)),
        review::checkbox(t, tick),
        Span::raw(format!("{:>SIZE$}", human(it.size))),
        Span::styled(format!("  {:>ITEMS$}", count(it.items)), t.fg_dim),
        Span::styled(
            format!("  {:>AGE$}  ", review::age(it.mtime, now)),
            t.fg_dim,
        ),
    ];
    let verdict = recommended(app, it, now);
    let chip = verdict.map_or("", chip);
    let chip_style = match verdict {
        Some(v) if v.recommended() => t.status_ok,
        Some(Verdict::InUse) => t.status_warn,
        _ => t.fg_dim,
    };
    let used: usize = spans.iter().map(Span::width).sum();
    let room = width.saturating_sub(used + CHIP + 3);
    let path = tilde(&it.path, app.home.as_deref());
    let shown = review::path(t, &path, shared, room, Style::new());
    let path_w: usize = shown.iter().map(Span::width).sum();
    spans.extend(shown);
    spans.push(Span::raw(" ".repeat(room.saturating_sub(path_w) + 2)));
    spans.push(Span::styled(format!("{chip:<CHIP$}"), chip_style));
    Line::from(spans)
}

/// The detail pane: the selected item's rule, what deleting it loses and why it is or is not
/// suggested at `now`. On a category, the rule of its first item; on a section, what its level
/// means.
fn detail_lines<'a>(app: &'a App, row: Option<&Row>, now: i64) -> Vec<Line<'a>> {
    let t = &app.theme;
    let i = match row {
        Some(&Row::Item(i)) => Some(i),
        Some(&Row::Group { cat, safety }) => group_items(app, cat, safety).first().copied(),
        Some(&Row::Section(safety)) => {
            let (name, meaning) = section_name(safety);
            let n = section_items(app, safety).len();
            return vec![
                Line::styled(name, t.title),
                Line::styled(
                    format!("{meaning}; {n} match{}", if n == 1 { "" } else { "es" }),
                    t.fg_dim,
                ),
            ];
        }
        None => None,
    };
    let (Some(i), Some(rules)) = (i, &app.snap.rules) else {
        return Vec::new();
    };
    let it = &app.extras.cleanup.items[i];
    let r = rules.rule(it.class.rule);
    let mut d = vec![
        Line::from(vec![
            Span::styled(r.name.clone(), t.title),
            Span::styled(format!(" · {}", it.class.safety.as_str()), t.fg_dim),
        ]),
        Line::raw(r.explanation.clone()),
        Line::styled(format!("You lose: {}", r.lost), t.fg_dim),
    ];
    if !matches!(row, Some(Row::Item(_))) {
        return d;
    }
    let warnings: Vec<&str> = [
        (ClassFlags::IN_USE, "in use"),
        (ClassFlags::VCS_TRACKED, "tracked by git"),
        (
            ClassFlags::VCS_UNKNOWN,
            "git could not tell if it is tracked",
        ),
    ]
    .into_iter()
    .filter(|&(flag, _)| it.class.has(flag))
    .map(|(_, what)| what)
    .collect();
    if !warnings.is_empty() {
        d.push(Line::styled(
            format!("Warning: {}.", warnings.join(", ")),
            t.status_warn,
        ));
    }
    if let Some(v) = recommended(app, it, now) {
        d.push(Line::styled(reason(v), t.fg_dim));
    }
    if r.action.native().is_some() {
        let key = first_key(Action::CleanupCommand);
        d.push(Line::styled(
            if app.read_only {
                format!("{key} shows its own cleanup command and runs its dry run")
            } else {
                format!("{key} runs its own cleanup command (dry run first)")
            },
            t.fg_dim,
        ));
    }
    d
}
