//! Drawing the view: the summary, the groups and items, the selected row's explanation, and the
//! native command pane over them.

use dfdu_engine::Verdict;
use dfdu_rules::Safety;
use dfdu_tree::{Class, now_secs};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{Item, Row, group_items, is_marked, markable, pane, plural, recommended, rows};
use crate::app::App;
use crate::model::outside::tilde;
use crate::theme::{count, human, truncate_middle};
use crate::view::cells;

/// Draws the view in `area`.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.snap.rules.is_none() {
        return;
    }
    let rows = rows(app);
    let mut lines = summary_lines(app);
    // The rule text below gives way to the list on short terminals: three rows stay for it.
    let summary_h = cells(lines.len());
    let detail_h: u16 = 4.min(area.height.saturating_sub(summary_h.saturating_add(1 + 3)));
    let list_h = usize::from(
        area.height
            .saturating_sub(summary_h.saturating_add(detail_h + 1)),
    );
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
    let columns = Columns::of(&rows);
    for (i, row) in rows.iter().enumerate().skip(scroll).take(list_h) {
        let mut spans = match *row {
            Row::Group { cat, safety } => group_line(app, cat, safety, columns),
            Row::Item(i) => item_line(app, &app.extras.cleanup.items[i], now, area.width),
        };
        if i == cursor {
            for s in &mut spans {
                s.style = s.style.patch(t.cursor);
            }
        }
        lines.push(Line::from(spans));
    }
    // The explanation goes in the space under the rows, one blank line below the last.
    let used = cells(lines.len()).min(area.height);
    let below = area.height - used;
    let top = Rect {
        height: used,
        ..area
    };
    let bottom = Rect {
        y: area.y + used + 1.min(below),
        height: below.saturating_sub(1),
        ..area
    };
    f.render_widget(Paragraph::new(lines), top);
    let detail = detail_lines(app, rows.get(cursor), now);
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), bottom);
    pane::draw(f, app, area, t);
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
    if let Some(rules) = &app.snap.rules {
        lines.extend(crate::model::outside::view_lines(app, rules));
    }
    lines
}

/// The widths of the group rows' text columns: as wide as the longest category and safety level
/// among the groups, so that every group's size lines up.
#[derive(Copy, Clone, Debug)]
struct Columns {
    category: usize,
    safety: usize,
}

impl Columns {
    fn of(rows: &[Row]) -> Self {
        let mut c = Columns {
            category: 0,
            safety: 0,
        };
        for row in rows {
            if let Row::Group { cat, safety } = *row {
                let name = dfdu_rules::CATEGORIES[cat as usize].name;
                c.category = c.category.max(Span::raw(name).width());
                c.safety = c.safety.max(Span::raw(safety.as_str()).width());
            }
        }
        c
    }
}

/// The row of the group of category `cat` at `safety`: its size, items and how many are marked.
/// The safety level tells apart the groups of a category found at two levels.
fn group_line(app: &App, cat: u16, safety: Safety, w: Columns) -> Vec<Span<'static>> {
    let t = &app.theme;
    let items = &app.extras.cleanup.items;
    let ix = group_items(app, cat, safety);
    let size: u64 = ix.iter().map(|&i| items[i].size).sum();
    let marked = ix.iter().filter(|&&i| is_marked(app, &items[i])).count();
    let category = &dfdu_rules::CATEGORIES[cat as usize];
    let open = app.cleanup.open.contains(&(cat, safety));
    vec![
        Span::raw(if open { " ▾ " } else { " ▸ " }),
        Span::styled(
            format!("{:<width$}", category.name, width = w.category),
            t.cat[category.color as usize],
        ),
        Span::raw(format!("  {:<width$}", safety.as_str(), width = w.safety)),
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

/// Why an item is or is not recommended, in a few words for its row; `None` for what the row's
/// warnings already say.
fn note(v: Verdict) -> Option<String> {
    Some(match v {
        Verdict::Safe => "recommended".into(),
        Verdict::Stale(d) => format!("older than {d} d"),
        Verdict::Level(Safety::Review) => "in review".into(),
        Verdict::Level(s) => s.as_str().into(),
        Verdict::InUse | Verdict::Tracked | Verdict::VcsUnknown => return None,
        Verdict::Scanning => "scanning".into(),
        Verdict::Newer(d) => format!("newer than {d} d"),
        Verdict::Below(min) => format!("below {}", human(min)),
    })
}

/// The same as a sentence, for the explanation under the rows.
fn reason(v: Verdict) -> String {
    match v {
        Verdict::Safe => "Recommended: safe to delete.".into(),
        Verdict::Stale(d) => format!("Recommended: unchanged for over {d} days."),
        Verdict::Level(s) => format!("Not recommended: {} items never are.", s.as_str()),
        Verdict::InUse => "Not recommended: in use.".into(),
        Verdict::Tracked => "Not recommended: tracked by git.".into(),
        Verdict::VcsUnknown => "Not recommended: git could not tell if it is tracked.".into(),
        Verdict::Scanning => "Not recommended until its scan finishes.".into(),
        Verdict::Newer(d) => format!("Not recommended: changed in the last {d} days."),
        Verdict::Below(min) => format!("Not recommended: below the rule's {}.", human(min)),
    }
}

/// The row of `it`, `width` cells wide: its mark, size, items, age in days at `now`, path,
/// warnings and why it is or is not recommended. The path is shortened in the middle to the room
/// the rest leaves.
fn item_line(app: &App, it: &Item, now: i64, width: u16) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mark = if is_marked(app, it) {
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
        Span::styled(format!("  {age:>4} d  "), t.fg_dim),
    ];
    let warnings: Vec<Span<'static>> = [
        (Class::IN_USE, "in use"),
        (Class::VCS_TRACKED, "git-tracked"),
        (Class::VCS_UNKNOWN, "git unknown"),
    ]
    .into_iter()
    .filter(|&(flag, _)| it.class.has(flag))
    .map(|(_, what)| Span::styled(format!("  {what}"), t.status_warn))
    .chain(
        recommended(app, it, now)
            .and_then(note)
            .map(|n| Span::styled(format!("  {n}"), t.fg_dim)),
    )
    .collect();
    let used: usize = v.iter().chain(&warnings).map(Span::width).sum();
    let path = tilde(&it.path, app.home.as_deref());
    let room = usize::from(width).saturating_sub(used);
    v.push(Span::raw(truncate_middle(&path, room)));
    v.extend(warnings);
    v
}

/// The selected item's rule: what it is, what deleting it loses and why it is or is not
/// recommended at `now`. On a group, the rule of its first item.
fn detail_lines<'a>(app: &'a App, row: Option<&Row>, now: i64) -> Vec<Line<'a>> {
    let t = &app.theme;
    let i = match row {
        Some(&Row::Item(i)) => Some(i),
        Some(&Row::Group { cat, safety }) => group_items(app, cat, safety).first().copied(),
        None => None,
    };
    let (Some(i), Some(rules)) = (i, &app.snap.rules) else {
        return Vec::new();
    };
    let it = &app.extras.cleanup.items[i];
    let r = rules.rule(it.class.rule);
    let mut d = vec![
        Line::from(vec![
            Span::styled(format!(" {}", r.name), t.title),
            Span::styled(format!("  {}", it.class.safety.as_str()), t.fg_dim),
        ]),
        Line::raw(format!(" {}", r.explanation)),
        Line::styled(format!(" Lost: {}", r.lost), t.fg_dim),
    ];
    if !matches!(row, Some(Row::Item(_))) {
        return d;
    }
    if let Some(v) = recommended(app, it, now) {
        d.push(Line::styled(format!(" {}", reason(v)), t.fg_dim));
    }
    if r.action.native().is_some() {
        d.push(Line::styled(
            if app.read_only {
                " n shows its own cleanup command and runs its dry run"
            } else {
                " n runs its own cleanup command"
            },
            t.fg_dim,
        ));
    }
    d
}
