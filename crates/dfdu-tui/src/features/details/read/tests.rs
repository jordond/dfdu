use std::time::Instant;

use dfdu_engine::SizeMode;
use dfdu_session::details::largest;
use dfdu_tree::Kind;

use super::*;
use crate::features::details::tests::{app_on, child, model};
use crate::theme::Theme;

/// The names and sizes `largest` lists.
///
/// # Panics
///
/// When it lists none.
fn listed(l: &Largest) -> Vec<(String, u64)> {
    let Largest::Listed(kids) = l else {
        panic!("nothing listed: {l:?}");
    };
    kids.iter().map(|k| (k.name.clone(), k.size)).collect()
}

/// The five largest children are listed largest first; a file lists none, and a directory
/// with more children than the limit is only counted.
#[test]
fn the_five_largest_children_are_listed_largest_first() {
    let m = model();
    let proj = child(&m, b"proj");
    let kids = listed(&largest::<100>(&m.tree, proj, SizeMode::Disk));
    assert_eq!(
        kids,
        [
            ("big.iso".to_owned(), 50_000),
            ("a.o".to_owned(), 12_000),
            ("b.o".to_owned(), 8_000),
            ("c.txt".to_owned(), 3_000),
            ("d.txt".to_owned(), 1_000),
        ]
    );
    let notes = child(&m, b"notes.txt");
    assert!(matches!(
        largest::<100>(&m.tree, notes, SizeMode::Disk),
        Largest::None
    ));
    assert!(matches!(
        largest::<8>(&m.tree, proj, SizeMode::Disk),
        Largest::Skipped(9)
    ));
}

/// The facts of the selected directory: what it is, and the mounts and read-only subvolumes
/// below it, which it is not itself.
#[test]
fn the_selected_entrys_facts_are_read() {
    let m = model();
    let app = app_on(&m);
    let proj = child(&m, b"proj");
    let info = read(&m, &app);
    assert_eq!(info.node, Some(m.tree.node_ref(proj)));
    let facts = info.facts.expect("facts of proj");
    assert_eq!(facts.name, "proj");
    assert_eq!(facts.path, m.tree.host_path(proj));
    assert!(facts.is_dir && facts.kind == Kind::Dir);
    assert_eq!(facts.inside, (1, 1), "the mount and the snapshot below it");
    assert!(facts.problem.is_some(), "its own error");
    assert_eq!(
        facts.errors_below, 1,
        "the directory that could not be read"
    );
    assert!(
        facts.matched.is_none() && facts.categories.is_empty(),
        "no rules"
    );
}

/// Nothing is read with the pane closed or nothing selected.
#[test]
fn a_closed_pane_or_no_selection_reads_nothing() {
    let m = model();
    let mut app = app_on(&m);
    app.session.details.open = false;
    let info = read(&m, &app);
    assert!(info.node.is_none() && info.facts.is_none());
    let mut empty = App::new(Theme::no_color(), Instant::now());
    empty.session.details.open = true;
    let info = read(&m, &empty);
    assert!(info.node.is_none() && info.facts.is_none());
}
