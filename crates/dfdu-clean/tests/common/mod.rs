//! Helpers the integration tests share: temporary directories and plan items.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use dfdu_clean::{Identity, PlanItem};
use dfdu_tree::{Meta, Tree};

/// A temporary directory under the canonical temporary directory, so paths the deleter
/// canonicalises compare equal to the ones built here.
pub fn canonical_tempdir() -> tempfile::TempDir {
    let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

/// The identity a scan would record for `p` (not following a symlink); `None` when it is gone.
/// On Windows that is what `dfdu-sys` reads (volume serial, file id), as the scanners and the
/// deleter's check do; std has neither.
pub fn identity(p: &Path) -> Option<Identity> {
    #[cfg(not(windows))]
    let m = Meta::from_metadata(&fs::symlink_metadata(p).ok()?);
    #[cfg(windows)]
    let m = Meta::from_windows(&dfdu_sys::stat_path(p, false).ok()?);
    Some(Identity {
        dev: m.dev,
        ino: m.ino,
        kind: m.kind,
        apparent: m.apparent,
        mtime: m.mtime,
    })
}

/// One plan item: a path and the identity it claims, on a node of its own in `tree`.
pub fn item(tree: &mut Tree, path: PathBuf, identity: Identity) -> PlanItem {
    let m = Meta::from_metadata(&fs::symlink_metadata("/").unwrap());
    let r = tree.new_root(b"/", &m, false);
    PlanItem {
        node: tree.node_ref(r),
        path,
        identity,
        bytes: 0,
        inside: std::sync::Arc::default(),
    }
}
