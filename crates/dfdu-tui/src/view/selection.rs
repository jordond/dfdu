//! The selection line: the status row of the list view when no message is showing. It says in
//! full what the selected row has only room for in short: its size and items, the data it
//! shares with what is outside the scan, its change since the last recorded scan and its
//! category, with the position in the list and what is marked on the right.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::list::name::cut_end;
use super::spread;
use crate::app::App;
use crate::model::growth;
use crate::state::marks;
use crate::theme::{count, human, truncate_middle};

/// `▸ name/  size · items · ↔ … · +Δ since … · category` on the left of `width` cells, the
/// position and the marked badge on the right; `None` when nothing is selected.
pub fn line(app: &App, width: u16) -> Option<Line<'static>> {
    let t = &app.theme;
    let sel = app.selected_index()?;
    let e = &app.snap.entries[app.rows[sel]];
    let mut right = Vec::new();
    if let Some(marked) = marks::summary(app) {
        right.push(marked);
        right.push(Span::raw("  "));
    }
    right.push(Span::styled(
        format!("{}/{} ", sel + 1, app.rows.len()),
        t.fg_dim,
    ));

    let dot = || Span::styled(" · ", t.fg_dim);
    let size = Span::raw(format!("  {}", human(e.size)));
    let items = (e.is_dir && e.items > 0).then(|| {
        let s = if e.items == 1 { "" } else { "s" };
        Span::styled(format!("{} item{s}", count(e.items)), t.fg_dim)
    });
    let mut rest = Vec::new();
    if e.shared_out > 0 {
        rest.push(Span::styled(
            format!("↔ {} also used outside the scan", human(e.shared_out)),
            t.fg_dim,
        ));
    }
    let path = app.snap.dir_path.join(dfdu_tree::name_os(&e.name));
    if app.growth.is_some()
        && !e.pseudo
        && let Some(n) = growth::row_note(app, &path, e.size, e.mark)
    {
        let style = if n.grew() { t.status_warn } else { t.fg_dim };
        rest.push(Span::styled(n.long(), style));
    }
    if let Some((g, rules)) = e.tag.zip(app.snap.rules.as_ref()) {
        let (name, slot) = g.category(rules);
        rest.push(Span::styled(
            name,
            if g.inherited { t.fg_dim } else { t.cat[slot] },
        ));
    }

    let width = usize::from(width);
    let room = width.saturating_sub(right.iter().map(Span::width).sum::<usize>() + 1);
    let lead = Span::styled(" ▸ ", t.key);
    let mut name = String::from_utf8_lossy(&e.name).into_owned();
    if e.is_dir {
        name.push('/');
    }
    let name_style = if e.is_dir { t.dir } else { Style::new() };
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    // The items go first when the line is too long, then what is after the size is cut.
    let mut parts: Vec<Span> = items.into_iter().chain(rest).collect();
    let full = lead.width()
        + Span::raw(&name).width()
        + size.width()
        + parts
            .iter()
            .map(|p| dot().width() + p.width())
            .sum::<usize>();
    if full > room && e.is_dir && e.items > 0 {
        parts.remove(0);
    }
    let name_room = room.saturating_sub(lead.width() + size.width());
    let mut left = vec![
        lead,
        Span::styled(truncate_middle(&name, name_room), name_style),
        size,
    ];
    for p in parts {
        let free = room.saturating_sub(width_of(&left));
        if dot().width() + p.width() <= free {
            left.push(dot());
            left.push(p);
            continue;
        }
        let cut = free.saturating_sub(dot().width());
        if cut >= 4 {
            left.push(dot());
            left.push(Span::styled(cut_end(&p.content, cut), p.style));
        }
        break;
    }
    Some(spread(
        left,
        right,
        u16::try_from(width).unwrap_or(u16::MAX),
    ))
}
