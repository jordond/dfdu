//! Helpers shared by the engine's integration tests. Each test crate uses only some of them.

#![allow(dead_code, reason = "each test crate uses only some of the helpers")]

pub mod files;
pub mod journal;
pub mod loaded;
pub mod portable;
pub mod remote;

use std::time::{Duration, Instant};

use dfdu_engine::{Engine, Model};
use dfdu_scan::Backend;
use dfdu_tree::{Ix, NodeRef, Tree};
use tempfile::TempDir;

/// A temporary directory under the canonical temp dir, so its path matches the scan root the
/// engine reports (on macOS the temp dir is behind the /var symlink).
pub fn tempdir() -> TempDir {
    tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

/// The child of `ix` named `name`.
pub fn child(tree: &Tree, ix: Ix, name: &str) -> Ix {
    tree.children(ix)
        .find(|&c| tree.name(c) == name.as_bytes())
        .unwrap_or_else(|| panic!("{name} missing"))
}

/// The node at `rel`, a `/`-separated path below the first scan root.
pub fn node<B: Backend>(e: &Engine<B>, rel: &str) -> NodeRef {
    e.read(|m| {
        let mut ix = m.tree.roots().next().unwrap();
        for part in rel.split('/') {
            ix = child(&m.tree, ix, part);
        }
        m.tree.node_ref(ix)
    })
}

/// Waits until `cond` holds of the model, for at most 20 s.
pub fn wait_for<B: Backend>(e: &Engine<B>, cond: impl Fn(&Model) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !e.read(&cond) {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
