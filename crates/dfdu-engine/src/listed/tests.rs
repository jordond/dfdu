use dfdu_rules::{Env, Platform, Safety};
use dfdu_tree::Kind;

use super::*;
use crate::test_util::meta;

/// A tree under construction, with the built-in rules of a Mac.
struct Caches {
    tree: Tree,
    rules: RuleSet,
    ino: u64,
}

impl Caches {
    fn dir(&mut self, parent: Ix, name: &str) -> Ix {
        self.ino += 1;
        let d = (self.tree).add_dir(parent, name.as_bytes(), &meta(Kind::Dir, 0, self.ino));
        let own = self.tree.own(d);
        self.tree.propagate_add(d, &own);
        self.tree.dir_mut(d).expect("directory").state = DirState::Complete;
        d
    }

    fn file(&mut self, parent: Ix, name: &str, size: u64) {
        self.ino += 1;
        let f = self.tree.add_file(
            parent,
            name.as_bytes(),
            &meta(Kind::File, size, self.ino),
            false,
            None,
        );
        let own = self.tree.own(f);
        self.tree.propagate_add(parent, &own);
    }

    fn class(&mut self, ix: Ix, id: &str, safety: Safety) {
        let rule = self.rules.index(id).expect(id);
        let class = Class {
            rule,
            safety,
            degraded: None,
            flags: 0,
        };
        self.tree.set_class(ix, Some(class));
    }

    fn find(&self, path: &str) -> Ix {
        let mut ix = self.tree.roots().next().unwrap();
        for name in path.split('/').filter(|n| !n.is_empty()) {
            ix = (self.tree.children(ix))
                .find(|&c| self.tree.name(c) == name.as_bytes())
                .unwrap_or_else(|| panic!("{path}"));
        }
        ix
    }
}

/// `/r/Caches` (the user caches) with `pip/` (a cache with a specific rule, 300 bytes),
/// `Yarn/` (another, below `JetBrains`-like `group/`, 200 bytes), `group/other` (50 bytes)
/// and `misc/` (1000 bytes, no rule of its own); `/r/target` a Rust target with
/// `node_modules/` inside, which its `stop_descent` keeps from being listed.
fn caches() -> Caches {
    let env = Env::with(Platform::MacOs, &[("HOME", "/Users/u")]);
    let mut fx = Caches {
        tree: Tree::new(),
        rules: dfdu_rules::builtin(&env).0,
        ino: 1,
    };
    let root = fx.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    fx.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let cache = fx.dir(root, "Caches");
    let pip = fx.dir(cache, "pip");
    fx.file(pip, "wheel", 300);
    let group = fx.dir(cache, "group");
    let yarn = fx.dir(group, "Yarn");
    fx.file(yarn, "pkg", 200);
    fx.file(group, "other", 50);
    let misc = fx.dir(cache, "misc");
    fx.file(misc, "blob", 1000);
    let target = fx.dir(root, "target");
    let nm = fx.dir(target, "node_modules");
    fx.file(nm, "x", 10);
    fx.class(cache, "sys.user-caches", Safety::Safe);
    fx.class(pip, "pip.cache", Safety::Safe);
    fx.class(yarn, "yarn.cache", Safety::Safe);
    fx.class(target, "rust.target", Safety::Regenerable);
    fx.class(nm, "node.node_modules", Safety::Regenerable);
    fx
}

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
    assert_eq!(l[0].path, PathBuf::from("/r/Caches"));
    // Equal sizes go by path.
    let yarn = fx.find("Caches/group/Yarn");
    fx.file(yarn, "more", 100);
    let l = listed(&fx.tree, &fx.rules);
    let pip = l.iter().position(|l| l.path.ends_with("pip")).unwrap();
    let yarn = l.iter().position(|l| l.path.ends_with("Yarn")).unwrap();
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
