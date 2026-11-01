//! Helpers for the tests of this crate and of the crates above it (the `test-support` feature):
//! the model of an empty scan of `/r`, and rows of a tree snapshot built by hand.

use dfdu_engine::Model;
use dfdu_engine::test_support::meta;
use dfdu_tree::{Ix, Kind};

use crate::snapshot::{EntryData, test_entry};

/// A model with the root `/r` and nothing in it.
pub fn root_model() -> (Model, Ix) {
    let mut m = Model::default();
    let root = m.tree.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    (m, root)
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
