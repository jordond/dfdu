//! Drawing the view: the summary, the groups and items, the selected row's explanation, and the
//! native command pane over them.

use dfdu_rules::Safety;
use dfdu_tree::{Class, now_secs};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{Item, Row, group_items, is_marked, markable, pane, plural, rows};
use crate::app::App;
use crate::model::outside::tilde;
use crate::theme::{count, human};
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

/// The row of the group of category `cat` at `safety`: its size, items and how many are marked.
fn group_line(app: &App, cat: u16, safety: Safety) -> Vec<Span<'static>> {
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
        Span::styled(format!("  {age:>4} d"), t.fg_dim),
        Span::raw(format!("  {}", tilde(&it.path, app.home.as_deref()))),
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
