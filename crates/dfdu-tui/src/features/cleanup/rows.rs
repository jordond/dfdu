//! The view's rows: a section per safety level, the categories in it and their items, and
//! which sections and categories are open.

use dfdu_rules::CategoryIx;
use dfdu_tree::Safety;

use super::Item;
use crate::app::App;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Row {
    /// Every category found at a safety level.
    Section(Safety),
    /// One category at one level.
    Group {
        cat: CategoryIx,
        safety: Safety,
    },
    Item(usize),
}

/// The rows: a section per safety level with matches, safest first; in each open section its
/// categories, largest first; each open category followed by its items.
pub(super) fn rows(app: &App) -> Vec<Row> {
    let Some(rules) = &app.snap.rules else {
        return vec![];
    };
    let items = &app.extras.cleanup.items;
    let mut groups: Vec<((CategoryIx, Safety), u64)> = vec![];
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
    let mut section = None;
    for ((cat, safety), _) in groups {
        if section != Some(safety) {
            section = Some(safety);
            out.push(Row::Section(safety));
        }
        if app.cleanup.closed.contains(&safety) {
            continue;
        }
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

/// The items at `safety`, as the filter lets them in.
pub(super) fn section_items(app: &App, safety: Safety) -> Vec<usize> {
    (app.extras.cleanup.items.iter().enumerate())
        .filter(|(_, it)| it.class.safety == safety)
        .map(|(i, _)| i)
        .collect()
}

pub(super) fn group_items(app: &App, cat: CategoryIx, safety: Safety) -> Vec<usize> {
    let Some(rules) = &app.snap.rules else {
        return vec![];
    };
    (app.extras.cleanup.items.iter().enumerate())
        .filter(|(_, it)| it.class.safety == safety && rules.rule(it.class.rule).category == cat)
        .map(|(i, _)| i)
        .collect()
}

/// What `Enter` does on the selected row, for the footer.
pub fn enter_does(app: &App) -> &'static str {
    match rows(app).get(app.cleanup.cursor) {
        Some(Row::Item(_)) => "show in Files",
        Some(&row) if is_open(app, row) => "close",
        _ => "open",
    }
}

pub(crate) fn selected_item(app: &App) -> Option<&Item> {
    match rows(app).get(app.cleanup.cursor)? {
        Row::Item(i) => app.extras.cleanup.items.get(*i),
        Row::Section(_) | Row::Group { .. } => None,
    }
}

/// Whether `row` shows what is below it.
pub(super) fn is_open(app: &App, row: Row) -> bool {
    match row {
        Row::Section(s) => !app.cleanup.closed.contains(&s),
        Row::Group { cat, safety } => app.cleanup.open.contains(&(cat, safety)),
        Row::Item(_) => false,
    }
}

/// Opens or closes `row`, a section or a category.
pub(super) fn set_open(app: &mut App, row: Row, open: bool) {
    let c = &mut app.cleanup;
    match row {
        Row::Section(s) if open => {
            c.closed.remove(&s);
        }
        Row::Section(s) => {
            c.closed.insert(s);
        }
        Row::Group { cat, safety } if open => {
            c.open.insert((cat, safety));
        }
        Row::Group { cat, safety } => {
            c.open.remove(&(cat, safety));
        }
        Row::Item(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::features::cleanup::tests::opened;
    use crate::features::treemap::tests::classified;

    /// The safety of each section and group row, and `Item` with the item's path for the rest.
    fn shown(app: &App) -> Vec<String> {
        (rows(app).iter())
            .map(|&r| match r {
                Row::Section(s) => format!("{s:?}"),
                Row::Group { safety, .. } => format!("  {safety:?}"),
                Row::Item(i) => {
                    let path = &app.extras.cleanup.items[i].path;
                    format!(
                        "    {}",
                        String::from_utf8_lossy(path.file_name().unwrap_or_default())
                    )
                }
            })
            .collect()
    }

    #[test]
    fn sections_are_open_and_categories_closed_by_default() {
        let fx = classified();
        let mut app = opened(&fx.m, Instant::now());
        assert_eq!(
            shown(&app),
            ["Safe", "  Safe", "Regenerable", "  Regenerable"]
        );
        assert_eq!(enter_does(&app), "close", "on the open section");
        let group = rows(&app)[3];
        assert!(!is_open(&app, group));
        set_open(&mut app, group, true);
        assert!(is_open(&app, group));
        assert_eq!(
            shown(&app),
            ["Safe", "  Safe", "Regenerable", "  Regenerable", "    big"]
        );
        app.cleanup.cursor = 4;
        assert_eq!(enter_does(&app), "show in Files");
        let item = selected_item(&app).expect("the item selected");
        assert!(item.path.to_local().ends_with("big"));
        // A closed section hides its categories.
        set_open(&mut app, Row::Section(Safety::Safe), false);
        assert_eq!(shown(&app)[..2], ["Safe", "Regenerable"]);
        app.cleanup.cursor = 0;
        assert_eq!(enter_does(&app), "open");
        assert!(selected_item(&app).is_none());
    }

    #[test]
    fn the_filter_keeps_the_safer_levels_and_items_go_by_group() {
        let fx = classified();
        let mut app = opened(&fx.m, Instant::now());
        let Row::Group { cat, safety } = rows(&app)[3] else {
            panic!("a group row");
        };
        assert_eq!(group_items(&app, cat, safety).len(), 1);
        assert_eq!(section_items(&app, Safety::Regenerable).len(), 1);
        assert_eq!(group_items(&app, cat, Safety::Safe), Vec::<usize>::new());
        app.cleanup.filter = Some(Safety::Safe);
        assert_eq!(shown(&app), ["Safe", "  Safe"]);
        // Without rules there is nothing to group.
        app.snap.rules = None;
        assert_eq!(rows(&app), []);
        assert_eq!(group_items(&app, cat, safety), Vec::<usize>::new());
    }
}
