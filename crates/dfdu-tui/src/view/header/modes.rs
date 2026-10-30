//! The modes on the right of the scope line: tree mode and the sort, always, then the size
//! shown and what differs from the defaults, each in a wide and a short form, the last giving
//! way first when the line is short.

use dfdu_engine::SizeMode;
use dfdu_store::history;
use ratatui::text::Span;

use crate::app::App;
use crate::model::growth;
use crate::state::filter;
use crate::state::order::SortKey;
use crate::theme::truncate_middle;

/// Between two modes on the right of the second line, on wide terminals and on narrow ones.
const WIDE_SEPARATOR: &str = "  ·  ";
const NARROW_SEPARATOR: &str = " · ";
/// The wide forms of the modes are used from this many columns, as in the mockups: the list
/// has its widest layout there too.
pub(super) const WIDE_FROM: u16 = 110;
/// At most this many cells of a filter query or a type filter in the header.
const QUERY_MAX: usize = 16;

/// One mode on the right of the second line, in its form for wide terminals and its short one.
struct Flag {
    wide: Vec<Span<'static>>,
    narrow: Vec<Span<'static>>,
}

impl Flag {
    /// A mode written `wide`, or `narrow` when the line is short.
    fn new(wide: impl Into<String>, narrow: impl Into<String>) -> Flag {
        Flag {
            wide: vec![Span::raw(wide.into())],
            narrow: vec![Span::raw(narrow.into())],
        }
    }
}

/// Tree mode, shown before the sort while the rows are a tree.
fn tree_flag(app: &App) -> Option<Flag> {
    app.tree_shown()?;
    let tree = vec![Span::styled("tree", app.theme.key)];
    Some(Flag {
        wide: tree.clone(),
        narrow: tree,
    })
}

/// The sort, always shown: `sort size ↓`, or `size ↓` when the line is short.
fn sort_flag(app: &App) -> Flag {
    let key = match app.sort.key {
        SortKey::Size => "size",
        SortKey::Name => "name",
        SortKey::Items => "items",
        SortKey::Mtime => "mtime",
    };
    let arrow = if app.sort.desc { "↓" } else { "↑" };
    let sort = format!("{key} {arrow}");
    Flag {
        wide: vec![
            Span::styled("sort ", app.theme.fg_dim),
            Span::raw(sort.clone()),
        ],
        narrow: vec![Span::raw(sort)],
    }
}

/// The modes after the sort, in the order they give way when the line is short (the last
/// first): in tree mode that the share is of the parent and what folds, the size shown, then
/// only what differs from the defaults:
/// hidden files hidden, the filter kept, the type filter, and the day of the last recorded scan
/// the changes are from.
fn flags(app: &App) -> Vec<Flag> {
    let mut out = Vec::new();
    if let Some(tree) = app.tree_shown() {
        out.push(Flag::new("share of parent", "of parent"));
        if let Some(fold) = crate::state::tree::act::fold_label(tree.fold_ratio) {
            out.push(Flag::new(fold, fold));
        }
    }
    out.push(match app.size_mode {
        SizeMode::Disk => Flag::new("disk usage", "disk"),
        SizeMode::Apparent => Flag::new("apparent size", "apparent"),
        SizeMode::Referenced => Flag::new("disk usage like du", "du-like"),
    });
    if app.hide_hidden {
        out.push(Flag::new("hiding hidden files", "hidden"));
    }
    if app.filter.active() && !filter::editing(app) {
        let query = format!("/{}", truncate_middle(app.filter.query(), QUERY_MAX));
        out.push(Flag::new(query.clone(), query));
    }
    if !app.snap.types.is_empty() {
        let types = truncate_middle(&app.snap.types.to_string(), QUERY_MAX);
        out.push(Flag::new(format!("type {types}"), format!("t {types}")));
    }
    // The size `du` counts is not recorded, so there are no changes to show then.
    if let Some(g) = &app.growth
        && app.size_mode != SizeMode::Referenced
    {
        out.push(Flag::new(
            format!("Δ since {}", history::day(g.at)),
            growth::column_title(app),
        ));
    }
    out
}

/// The right of the second line in the forms to try, longest first: every mode in its wide
/// form (used from [`WIDE_FROM`] columns), then every mode short, then short with the last
/// modes left out and `+N` in their place. Tree mode and the sort are never left out.
pub(super) fn mode_forms(app: &App) -> Vec<Vec<Span<'static>>> {
    let t = &app.theme;
    let lead: Vec<Flag> = tree_flag(app).into_iter().chain([sort_flag(app)]).collect();
    let rest = flags(app);
    let join = |parts: Vec<Vec<Span<'static>>>, sep: &'static str| {
        let mut spans = Vec::new();
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(sep, t.fg_dim));
            }
            spans.extend(part);
        }
        spans.push(Span::raw(" "));
        spans
    };
    let wide = lead.iter().chain(&rest).map(|f| f.wide.clone()).collect();
    let mut forms = vec![join(wide, WIDE_SEPARATOR)];
    for kept in (0..=rest.len()).rev() {
        let mut parts: Vec<_> = lead
            .iter()
            .chain(&rest[..kept])
            .map(|f| f.narrow.clone())
            .collect();
        let dropped = rest.len() - kept;
        if dropped > 0 {
            parts.push(vec![Span::styled(format!("+{dropped}"), t.fg_dim)]);
        }
        forms.push(join(parts, NARROW_SEPARATOR));
    }
    forms
}

#[cfg(test)]
mod tests;
