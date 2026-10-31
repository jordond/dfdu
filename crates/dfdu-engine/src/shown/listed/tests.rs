use super::*;
use crate::test_support::{Caches, caches};

/// The entries' paths, written with `/` whatever this host's separator is.
fn names(fx: &Caches, ixs: &[Ix]) -> Vec<String> {
    let mut v: Vec<String> = ixs
        .iter()
        .map(|&i| String::from_utf8_lossy(&fx.tree.path(i)).replace(std::path::MAIN_SEPARATOR, "/"))
        .collect();
    v.sort();
    v
}

#[test]
fn a_container_yields_to_the_matches_inside_it() {
    let fx = caches();
    let l = listed(&fx.tree, &fx.rules);
    let shown: Vec<Ix> = l.iter().map(|l| l.ix).collect();
    assert_eq!(
        names(&fx, &shown),
        [
            "/r/Caches",
            "/r/Caches/group/Yarn",
            "/r/Caches/pip",
            "/r/target"
        ]
    );
    let cache = l.iter().find(|l| l.ix == fx.find("Caches")).unwrap();
    assert_eq!(
        names(&fx, &cache.rest),
        ["/r/Caches/group/other", "/r/Caches/misc"]
    );
    // Without pip and Yarn; every byte counted once.
    let own = |p: &str| fx.tree.own(fx.find(p)).alloc;
    assert_eq!(
        cache.total.alloc,
        1050 + own("Caches") + own("Caches/group") + own("Caches/misc")
    );
    let sum: u64 = l.iter().map(|l| l.total.alloc).sum();
    let root = fx.tree.roots().next().unwrap();
    assert_eq!(sum, fx.tree.total(root).alloc - fx.tree.own(root).alloc);
    let target = l.iter().find(|l| l.ix == fx.find("target")).unwrap();
    assert!(target.inner.is_empty() && target.rest.is_empty());
    assert!(!target.scanning);
}

#[test]
fn the_largest_come_first() {
    let mut fx = caches();
    let l = listed(&fx.tree, &fx.rules);
    let sizes: Vec<u64> = l.iter().map(|l| l.total.alloc).collect();
    assert!(sizes.is_sorted_by(|a, b| a >= b), "{sizes:?}");
    assert_eq!(l[0].path, dfdu_tree::HostPath::from("/r").join(b"Caches"));
    // Equal sizes go by path.
    let yarn = fx.find("Caches/group/Yarn");
    fx.file(yarn, "more", 100);
    let l = listed(&fx.tree, &fx.rules);
    let pip = l
        .iter()
        .position(|l| l.path.file_name() == Some(b"pip"))
        .unwrap();
    let yarn = l
        .iter()
        .position(|l| l.path.file_name() == Some(b"Yarn"))
        .unwrap();
    assert_eq!(l[yarn].total.alloc, l[pip].total.alloc);
    assert!(yarn < pip, "{l:?}");
}

#[test]
fn a_container_holding_only_matches_is_not_listed() {
    let mut fx = caches();
    for p in ["Caches/misc", "Caches/group"] {
        let ix = fx.find(p);
        fx.tree.detach(ix);
    }
    let l = listed(&fx.tree, &fx.rules);
    let shown: Vec<Ix> = l.iter().map(|l| l.ix).collect();
    assert_eq!(names(&fx, &shown), ["/r/Caches/pip", "/r/target"]);
}

/// What the cleanup view lists is what every other view says covers an entry: pip's cache in
/// the user caches is pip's cache, `node_modules` in a Rust target is the target.
#[test]
fn listed_matches_are_the_classes_in_effect() {
    let fx = caches();
    let in_effect = |p: &str| {
        let (at, c) = fx.rules.class_in_effect(&fx.tree, fx.find(p)).expect(p);
        (at, fx.rules.rule(c.rule).id.as_str())
    };
    assert_eq!(
        in_effect("Caches/pip/wheel"),
        (fx.find("Caches/pip"), "pip.cache")
    );
    assert_eq!(
        in_effect("Caches/misc"),
        (fx.find("Caches"), "sys.user-caches")
    );
    assert_eq!(
        in_effect("target/node_modules/x"),
        (fx.find("target"), "rust.target")
    );
    let l = listed(&fx.tree, &fx.rules);
    for (ix, _) in fx.tree.classes() {
        let shown = l.iter().any(|l| l.ix == ix);
        let (at, _) = fx.rules.class_in_effect(&fx.tree, ix).expect("classified");
        // The user caches hold other things besides their matches, so they are listed too.
        assert_eq!(
            shown,
            at == ix,
            "{}",
            String::from_utf8_lossy(&fx.tree.path(ix))
        );
    }
}
