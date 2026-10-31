//! The parts of a list row that tree mode draws its own way: the guide and `▸`/`▾` before the
//! name, the bar with a shadow up to its parent's length, the share of the parent, and the
//! fold rows (`… 12 more`), dim and in italics.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::model::snapshot::EntryData;
use crate::state::{motion, tree};
use crate::theme::{bar_with_shadow, cut_end, percent, ratio};

/// What a row of a tree needs besides its entry.
pub(super) struct TreeRow {
    /// The `├─ └─ │` before the name, from [`tree::guide`].
    guide: String,
    /// The entry of the row it is in; `None` at the top.
    parent: Option<usize>,
}

/// The tree parts of the rows `visible` (entry indices), or `None` when the rows are not a
/// tree.
pub(super) fn rows(app: &App, visible: &[usize]) -> Option<Vec<TreeRow>> {
    app.tree_shown()?;
    let entries = &app.snap.entries;
    let last = tree::last_children(entries, &app.rows);
    Some(
        visible
            .iter()
            .map(|&i| TreeRow {
                guide: tree::guide(entries, &last, i),
                parent: tree::parent(entries, i),
            })
            .collect(),
    )
}

/// The guide and the `▸`/`▾` before the name of `e`, for a name cell `width` wide; nothing
/// before a fold row's text but the guide. A guide that would take more than half the cell
/// keeps its end, after a `…`.
pub(super) fn prefix(app: &App, e: &EntryData, row: &TreeRow, width: usize) -> Vec<Span<'static>> {
    let t = &app.theme;
    let glyph = if e.fold > 0 {
        ""
    } else if e.is_dir && !e.pseudo {
        let open = app.tree.as_ref().is_some_and(|tr| tr.is_expanded(e.node));
        if open { "▾ " } else { "▸ " }
    } else {
        "  "
    };
    let room = (width / 2).saturating_sub(glyph.chars().count());
    let cells = row.guide.chars().count();
    let guide = if cells <= room {
        row.guide.clone()
    } else {
        let keep = room.saturating_sub(1);
        let tail: String = row.guide.chars().skip(cells - keep).collect();
        format!("…{tail}")
    };
    let mut spans = Vec::with_capacity(2);
    if !guide.is_empty() {
        spans.push(Span::styled(guide, t.fg_dim));
    }
    if !glyph.is_empty() {
        spans.push(Span::styled(glyph, t.key));
    }
    spans
}

/// The text of a fold row, `width` cells at most: dim and in italics.
pub(super) fn fold_name(app: &App, e: &EntryData, width: usize) -> Span<'static> {
    let name = String::from_utf8_lossy(&e.name);
    let style = app.theme.fg_dim.add_modifier(Modifier::ITALIC);
    Span::styled(cut_end(&name, width), style)
}

/// The bar of `e` on the scale where `max` is a full bar, its own length in the bar colour,
/// then a shadow up to its parent's length.
pub(super) fn bar(
    app: &App,
    e: &EntryData,
    row: &TreeRow,
    max: u64,
    width: u16,
) -> [Span<'static>; 2] {
    let t = &app.theme;
    let own = motion::bar(app, e, ratio(e.size, max));
    let parent = row.parent.map(|p| ratio(app.snap.entries[p].size, max));
    let (filled, shadow) = bar_with_shadow(own, parent, width.into(), t.bars);
    [
        Span::styled(filled, t.bar_fill),
        Span::styled(shadow, t.bar_empty),
    ]
}

/// The share of `e` in its parent's size, or in the directory's `total` at the top.
pub(super) fn share(app: &App, e: &EntryData, row: &TreeRow, total: u64) -> String {
    let whole = row
        .parent
        .map_or(total, |p| app.snap.entries[p].size)
        .max(1);
    format!("{:.1}%", percent(e.size, whole))
}

/// The size of a fold row is dim, like its text.
pub(super) fn size_style(app: &App, e: &EntryData, base: Style) -> Style {
    if e.fold > 0 {
        base.patch(app.theme.fg_dim)
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::state::tree::TreeState;
    use crate::state::tree::tests::sample_with_files;
    use crate::tests::snap_with_totals;
    use crate::theme::Theme;

    /// A browser in tree mode of [`sample`], `y` (row 2) expanded and the leaves files.
    fn browser() -> App {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let entries = sample_with_files();
        let mut tree = TreeState::default();
        tree.expand(dfdu_tree::Target {
            node: Some(entries[2].node),
            path: "/r/a/y".into(),
            ident: None,
        });
        app.tree = Some(tree);
        app.refresh(snap_with_totals(entries, 1), t0);
        app.reorder(t0);
        app
    }

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn rows_have_their_guide_and_parent_only_in_a_tree() {
        let mut app = browser();
        let rows = rows(&app, &[0, 1, 3, 8]).expect("a tree");
        let guides: Vec<&str> = rows.iter().map(|r| r.guide.as_str()).collect();
        assert_eq!(guides, ["", "├─", "│ ├─", "  └─"]);
        let parents: Vec<Option<usize>> = rows.iter().map(|r| r.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(2), Some(7)]);
        app.tree = None;
        assert!(super::rows(&app, &[0]).is_none());
    }

    #[test]
    fn the_glyph_says_whether_a_directory_is_open() {
        let app = browser();
        let es = &app.snap.entries;
        let rows = rows(&app, &[0, 2, 3]).expect("a tree");
        let shown = |i: usize, row: &TreeRow, width| text(&prefix(&app, &es[i], row, width));
        assert_eq!(shown(0, &rows[0], 40), "▸ ");
        assert_eq!(shown(2, &rows[1], 40), "├─▾ ");
        assert_eq!(shown(3, &rows[2], 40), "│ ├─  ");
        // A guide longer than half the cell keeps its end.
        assert_eq!(shown(3, &rows[2], 10), "…├─  ");
    }

    #[test]
    fn shares_are_of_the_parent_and_fold_rows_are_dim() {
        let mut app = browser();
        let rows = rows(&app, &[0, 1]).expect("a tree");
        let es = app.snap.entries.clone();
        assert_eq!(share(&app, &es[1], &rows[1], 1000), "50.0%", "x of a");
        assert_eq!(
            share(&app, &es[0], &rows[0], 1000),
            "10.0%",
            "a of the directory"
        );
        let [filled, shadow] = bar(&app, &es[1], &rows[1], 100, 10);
        assert_eq!(filled.width() + shadow.width(), 10);
        assert!(
            filled.width() < 10,
            "the shadow runs on to the parent's length"
        );
        let base = Style::new();
        assert_eq!(size_style(&app, &es[1], base), base);
        app.snap.entries[1].fold = 3;
        app.snap.entries[1].name = "… 3 more".as_bytes().into();
        let fold = &app.snap.entries[1];
        assert_eq!(size_style(&app, fold, base), base.patch(app.theme.fg_dim));
        let name = fold_name(&app, fold, 4);
        assert_eq!(name.content, "… 3…");
        assert!(name.style.add_modifier.contains(Modifier::ITALIC));
    }
}
