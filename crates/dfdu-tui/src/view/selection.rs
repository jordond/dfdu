//! The selection line: the status row of the list view when no message is showing. It says in
//! full what the selected row has only room for in short: its size and items, the data it
//! shares with what is outside the scan, its change since the last recorded scan and its
//! category, with the position in the list and what is marked on the right. In tree mode it
//! starts with the row's path from the scan root and its share of the directory it is in.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::list::name::cut_end;
use super::spread;
use crate::app::App;
use crate::model::growth;
use crate::model::outside::{rel_text, tilde};
use crate::state::{marks, tree};
use crate::theme::{count, human, percent, truncate_middle};

/// `▸ name/  size · items · ↔ … · +Δ since … · category` on the left of `width` cells, the
/// position and the marked badge on the right; `None` when nothing is selected.
pub fn line(app: &App, width: u16) -> Option<Line<'static>> {
    let t = &app.theme;
    let sel = app.selected_index()?;
    let index = app.rows[sel];
    let e = &app.snap.entries[index];
    let in_tree = app.tree_shown().is_some();
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
    let share = in_tree.then(|| share_of_parent(app, index));
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
    let path = tree::path_of(&app.snap, index);
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
    let slash = |mut s: String| {
        if e.is_dir {
            s.push('/');
        }
        s
    };
    // In tree mode the path from the scan root, or all of it when that fits.
    let names: Vec<String> = if in_tree && e.fold == 0 {
        let rel = rel_text(&path, &app.snap.root_path).unwrap_or_else(|| path.to_string());
        vec![slash(tilde(&path, app.home.as_deref())), slash(rel)]
    } else {
        vec![slash(String::from_utf8_lossy(&e.name).into_owned())]
    };
    let name_style = if e.is_dir { t.dir } else { Style::new() };
    let width_of = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    // The items go first when the line is too long, then what is after the size is cut.
    let at_items = usize::from(share.is_some());
    let mut parts: Vec<Span> = share.into_iter().chain(items).chain(rest).collect();
    let name_room = room.saturating_sub(lead.width() + size.width());
    let full = |name: &str| {
        lead.width()
            + Span::raw(name).width()
            + size.width()
            + parts
                .iter()
                .map(|p| dot().width() + p.width())
                .sum::<usize>()
    };
    // The longer path only when the whole line fits with it.
    let name = names
        .iter()
        .find(|n| full(n) <= room)
        .unwrap_or(&names[names.len() - 1]);
    if full(name) > room && e.is_dir && e.items > 0 {
        parts.remove(at_items);
    }
    let mut left = vec![
        lead,
        Span::styled(truncate_middle(name, name_room), name_style),
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

/// `42.5% of dfdu/`: the share of entry `index` in the row it is in, or at the top in the
/// current directory.
fn share_of_parent(app: &App, index: usize) -> Span<'static> {
    let s = &app.snap;
    let (whole, name) = match tree::parent(&s.entries, index) {
        Some(p) => (
            s.entries[p].size,
            String::from_utf8_lossy(&s.entries[p].name).into_owned(),
        ),
        None => (
            app.size_mode.of(&s.dir_total),
            s.dir_path.file_name().map_or_else(
                || s.dir_path.to_string(),
                |n| String::from_utf8_lossy(n).into_owned(),
            ),
        ),
    };
    let share = percent(s.entries[index].size, whole.max(1));
    let slash = if name.ends_with('/') { "" } else { "/" };
    Span::styled(format!("{share:.1}% of {name}{slash}"), app.theme.fg_dim)
}
