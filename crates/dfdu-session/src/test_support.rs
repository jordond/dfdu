//! Helpers for the tests of this crate and of the crates above it (the `test-support` feature):
//! the model of an empty scan of `/r`, a model built by hand ([`Fixture`], [`tree`],
//! [`classified`]), and rows of a tree snapshot built by hand.

use std::sync::Arc;

use dfdu_engine::Model;
use dfdu_engine::test_support::{add_dir, add_file, meta, node_at};
use dfdu_tree::{DirState, Ix, Kind};

use crate::snapshot::{EntryData, test_entry};

/// A model with the root `/r` and nothing in it.
pub fn root_model() -> (Model, Ix) {
    let mut m = Model::default();
    let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    (m, root)
}

/// A model built by hand, with the inode the next entry gets.
pub struct Fixture {
    pub m: Model,
    ino: u64,
}

impl Fixture {
    /// [`root_model`], and its root.
    pub fn new() -> (Self, Ix) {
        let (m, root) = root_model();
        (Fixture { m, ino: 1 }, root)
    }

    pub fn dir(&mut self, parent: Ix, name: &str) -> Ix {
        self.ino += 1;
        add_dir(&mut self.m.tree, parent, name, self.ino)
    }

    pub fn file(&mut self, parent: Ix, name: &str, size: u64) -> Ix {
        self.ino += 1;
        add_file(&mut self.m.tree, parent, name, size, self.ino)
    }
}

/// `/r` with `big/` (`a/` with two files, and `b`), `mid/` (three files), `small` and dotfile
/// `.hid`.
pub fn tree() -> Fixture {
    let (mut fx, root) = Fixture::new();
    fx.m.tree.dir_mut(root).expect("root").state = DirState::Complete;
    let big = fx.dir(root, "big");
    let a = fx.dir(big, "a");
    fx.file(a, "a1", 30_000);
    fx.file(a, "a2", 10_000);
    fx.file(big, "b", 20_000);
    let mid = fx.dir(root, "mid");
    for (i, size) in [12_000, 8_000, 5_000].into_iter().enumerate() {
        fx.file(mid, &format!("m{i}"), size);
    }
    fx.file(root, "small", 9_000);
    fx.file(root, ".hid", 4_000);
    fx
}

/// [`tree`] with the built-in rules, `big` matched as a Rust `target/` and `mid/m0` as a
/// directory with a `CACHEDIR.TAG`.
pub fn classified() -> Fixture {
    let mut fx = tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let rules = dfdu_rules::builtin(&env).0;
    let class = dfdu_tree::Class {
        rule: rules.index("rust.target").expect("rust.target"),
        safety: dfdu_tree::Safety::Regenerable,
        degraded: None,
        flags: dfdu_tree::ClassFlags::NONE,
    };
    let cachedir = dfdu_tree::Class {
        rule: rules
            .index("generic.cachedir-tag")
            .expect("generic.cachedir-tag"),
        safety: dfdu_tree::Safety::Safe,
        ..class
    };
    fx.m.rules = Some(Arc::new(rules));
    let (big, m0) = (node_at(&fx.m.tree, "big"), node_at(&fx.m.tree, "mid/m0"));
    fx.m.tree.set_class(big, Some(class));
    fx.m.tree.set_class(m0, Some(cachedir));
    fx
}

/// Row `i` named `name` of `size` bytes, below row `parent`.
pub fn row(
    i: u32,
    name: &str,
    size: u64,
    parent: Option<usize>,
    entries: &[EntryData],
) -> EntryData {
    let (depth, parent) = match parent {
        Some(p) => (entries[p].depth + 1, u32::try_from(p).expect("small")),
        None => (0, u32::MAX),
    };
    EntryData {
        is_dir: true,
        depth,
        parent,
        ..test_entry(i, name, size)
    }
}

/// ```text
/// a
/// ├─x
/// ├─y
/// │ ├─p
/// │ └─q
/// └─z
/// b
/// └─only
///   └─deep
/// ```
pub fn sample() -> Vec<EntryData> {
    let mut es: Vec<EntryData> = Vec::new();
    for (name, size, parent) in [
        ("a", 100, None),
        ("x", 50, Some(0)),
        ("y", 40, Some(0)),
        ("p", 30, Some(2)),
        ("q", 10, Some(2)),
        ("z", 10, Some(0)),
        ("b", 90, None),
        ("only", 90, Some(6)),
        ("deep", 90, Some(7)),
    ] {
        let i = u32::try_from(es.len()).expect("small");
        let e = row(i, name, size, parent, &es);
        es.push(e);
    }
    es
}

/// [`sample`] with its leaves files: only `a`, `y`, `b` and `only` are directories.
pub fn sample_with_files() -> Vec<EntryData> {
    let mut es = sample();
    for e in &mut es {
        e.is_dir = ["a", "y", "b", "only"]
            .iter()
            .any(|d| e.name[..] == *d.as_bytes());
    }
    es
}
