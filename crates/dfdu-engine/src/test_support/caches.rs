//! The user caches with caches of their own inside, and a Rust target: the matches the cleanup
//! view and the details pane show for a container that yields to the matches inside it
//! ([`listed`](crate::listed)).

use dfdu_rules::{Env, Platform, RuleSet};
use dfdu_tree::{Class, DirState, Ix, Kind, Safety, Tree};

use super::{add_dir, add_file, meta, node_at};

/// A tree under construction, with the built-in rules of a Mac.
pub struct Caches {
    pub tree: Tree,
    pub rules: RuleSet,
    ino: u64,
}

impl Caches {
    /// Adds directory `name` to `parent`, read to the end.
    pub fn dir(&mut self, parent: Ix, name: &str) -> Ix {
        self.ino += 1;
        add_dir(&mut self.tree, parent, name, self.ino)
    }

    /// Adds file `name` of `size` bytes to `parent`.
    pub fn file(&mut self, parent: Ix, name: &str, size: u64) -> Ix {
        self.ino += 1;
        add_file(&mut self.tree, parent, name, size, self.ino)
    }

    /// Classifies `ix` as a match of rule `id` at `safety`.
    ///
    /// # Panics
    ///
    /// When the built-in rules have no rule `id`.
    pub fn class(&mut self, ix: Ix, id: &str, safety: Safety) {
        let rule = self.rules.index(id).expect(id);
        let class = Class {
            rule,
            safety,
            degraded: None,
            flags: dfdu_tree::ClassFlags::NONE,
        };
        self.tree.set_class(ix, Some(class));
    }

    /// The node at `path` below the root (`""` for the root).
    ///
    /// # Panics
    ///
    /// When there is no such node.
    #[must_use]
    pub fn find(&self, path: &str) -> Ix {
        node_at(&self.tree, path)
    }
}

/// `/r/Caches` (the user caches) with `pip/` (a cache with a specific rule, 300 bytes),
/// `Yarn/` (another, below `JetBrains`-like `group/`, 200 bytes), `group/other` (50 bytes)
/// and `misc/` (1000 bytes, no rule of its own); `/r/target` a Rust target with
/// `node_modules/` inside, which its `stop_descent` keeps from being listed.
///
/// # Panics
///
/// When the built-in rules of a Mac lack one of the rules it classifies with.
#[must_use]
pub fn caches() -> Caches {
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
