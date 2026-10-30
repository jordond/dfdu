use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::features::details::tests::{app_on, child, model};
use crate::tests::caches::caches;
use crate::theme::Theme;

/// The names and sizes `largest` lists, or why it lists none.
fn listed(l: &Largest) -> Result<Vec<(String, u64)>, String> {
    match l {
        Largest::Listed(kids) => Ok(kids.iter().map(|k| (k.name.clone(), k.size)).collect()),
        other => Err(format!("{other:?}")),
    }
}

/// The five largest children are listed largest first; a file lists none, and a directory
/// with more children than the limit is only counted.
#[test]
fn the_five_largest_children_are_listed_largest_first() {
    let m = model();
    let proj = child(&m, b"proj");
    let kids = listed(&largest::<100>(&m.tree, proj, SizeMode::Disk)).unwrap();
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
    app.details.open = false;
    let info = read(&m, &app);
    assert!(info.node.is_none() && info.facts.is_none());
    let mut empty = App::new(Theme::no_color(), Instant::now());
    empty.details.open = true;
    let info = read(&m, &empty);
    assert!(info.node.is_none() && info.facts.is_none());
}

/// The pane names the rule the cleanup view lists an entry under: pip's cache inside the user
/// caches is pip's cache, and a file in it is in pip's cache.
#[test]
fn the_pane_names_the_nearest_match_like_the_cleanup_view() {
    let fx = caches();
    let (pip, wheel) = (fx.find("Caches/pip"), fx.find("Caches/pip/wheel"));
    let nm = fx.find("target/node_modules");
    let name = |id: &str| fx.rules.get(id).expect(id).name.clone();
    let (pip_name, target_name) = (name("pip.cache"), name("rust.target"));
    let m = Model {
        tree: fx.tree,
        rules: Some(Arc::new(fx.rules)),
        ..Model::default()
    };
    let at = matched(&m, pip).expect("pip");
    assert_eq!((at.rule.as_str(), at.above), (pip_name.as_str(), None));
    let at = matched(&m, wheel).expect("wheel");
    assert_eq!(at.rule, pip_name);
    assert_eq!(at.above, Some(m.tree.host_path(pip)));
    // A match below a rule that stops the descent is covered by that one.
    let at = matched(&m, nm).expect("node_modules");
    assert_eq!(at.rule, target_name);
}

/// Below a directory, the matches no other match covers are summed by category, largest
/// first; at or inside a match, it alone is counted.
#[test]
fn categories_sum_the_matches_no_other_covers() {
    let fx = caches();
    let (root, caches_dir) = (fx.find(""), fx.find("Caches"));
    let (pip, target) = (fx.find("Caches/pip"), fx.find("target"));
    let category = |id: &str| {
        let rule = fx.rules.index(id).expect(id);
        fx.rules.rule(rule).category().name
    };
    let (user_caches, rust) = (category("sys.user-caches"), category("rust.target"));
    let pip_cat = category("pip.cache");
    let m = Model {
        tree: fx.tree,
        rules: Some(Arc::new(fx.rules)),
        ..Model::default()
    };
    let size = |ix| SizeMode::Disk.of(&m.tree.total(ix));
    let sums: Vec<(&str, u64, u64)> = categories(&m, root, SizeMode::Disk)
        .into_iter()
        .map(|(name, _, size, n)| (name, size, n))
        .collect();
    let mut want = vec![(user_caches, size(caches_dir), 1), (rust, size(target), 1)];
    if user_caches == rust {
        want = vec![(rust, size(caches_dir) + size(target), 2)];
    }
    assert_eq!(sums, want);
    let at_pip: Vec<(&str, u64)> = categories(&m, pip, SizeMode::Disk)
        .into_iter()
        .map(|(name, _, size, _)| (name, size))
        .collect();
    assert_eq!(at_pip, [(pip_cat, size(pip))]);
}
