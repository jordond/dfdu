//! The view's rows: a section per safety level, the categories in it and their items, and
//! which sections and categories are open.

use dfdu_rules::CategoryIx;
use dfdu_tree::Safety;

use super::Item;
use crate::state::SessionState;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Every category found at a safety level.
    Section(Safety),
    /// One category at one level.
    Group { cat: CategoryIx, safety: Safety },
    /// An item, by its index in the read.
    Item(usize),
}

/// The rows: a section per safety level with matches, safest first; in each open section its
/// categories, largest first; each open category followed by its items.
pub fn rows(s: &SessionState) -> Vec<Row> {
    let Some(rules) = &s.snap.rules else {
        return vec![];
    };
    let items = &s.extras.cleanup.items;
    let mut groups: Vec<((CategoryIx, Safety), u64)> = vec![];
    for it in items {
        if s.cleanup.filter.is_some_and(|f| it.class.safety > f) {
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
        if s.cleanup.closed.contains(&safety) {
            continue;
        }
        out.push(Row::Group { cat, safety });
        if s.cleanup.open.contains(&(cat, safety)) {
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
pub fn section_items(s: &SessionState, safety: Safety) -> Vec<usize> {
    (s.extras.cleanup.items.iter().enumerate())
        .filter(|(_, it)| it.class.safety == safety)
        .map(|(i, _)| i)
        .collect()
}

/// The items of category `cat` at `safety`.
pub fn group_items(s: &SessionState, cat: CategoryIx, safety: Safety) -> Vec<usize> {
    let Some(rules) = &s.snap.rules else {
        return vec![];
    };
    (s.extras.cleanup.items.iter().enumerate())
        .filter(|(_, it)| it.class.safety == safety && rules.rule(it.class.rule).category == cat)
        .map(|(i, _)| i)
        .collect()
}

/// What activating the selected row does, for the footer.
pub fn enter_does(s: &SessionState) -> &'static str {
    match rows(s).get(s.cleanup.cursor) {
        Some(Row::Item(_)) => "show in Files",
        Some(&row) if is_open(s, row) => "close",
        _ => "open",
    }
}

/// The selected item; `None` on a section or category.
pub fn selected(s: &SessionState) -> Option<&Item> {
    match rows(s).get(s.cleanup.cursor)? {
        Row::Item(i) => s.extras.cleanup.items.get(*i),
        Row::Section(_) | Row::Group { .. } => None,
    }
}

/// Whether `row` shows what is below it.
pub fn is_open(s: &SessionState, row: Row) -> bool {
    match row {
        Row::Section(safety) => !s.cleanup.closed.contains(&safety),
        Row::Group { cat, safety } => s.cleanup.open.contains(&(cat, safety)),
        Row::Item(_) => false,
    }
}

/// Opens or closes `row`, a section or a category.
pub fn set_open(s: &mut SessionState, row: Row, open: bool) {
    let c = &mut s.cleanup;
    match row {
        Row::Section(safety) if open => {
            c.closed.remove(&safety);
        }
        Row::Section(safety) => {
            c.closed.insert(safety);
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
