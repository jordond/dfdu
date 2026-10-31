//! The user caches with caches of their own inside, and a Rust target: the matches the cleanup
//! view and the details pane show for a container that yields to the matches inside it
//! ([`dfdu_engine::listed`]).

use dfdu_rules::{Env, Platform, RuleSet};

use dfdu_tree::{Class, DirState, Ix, Kind, Safety, Tree};

use super::meta;

/// A tree under construction, with the built-in rules of a Mac.
pub(crate) struct Caches {
    pub(crate) tree: Tree,
    pub(crate) rules: RuleSet,
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

    pub(crate) fn find(&self, path: &str) -> Ix {
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
pub(crate) fn caches() -> Caches {
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
