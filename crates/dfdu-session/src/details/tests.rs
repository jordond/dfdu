use std::sync::Arc;

use dfdu_engine::test_support::caches;

use super::*;

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
