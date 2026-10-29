//! Helpers shared by the unit tests of several modules.

use dfdu_tree::{Kind, Meta};

/// The metadata of a node of a test tree: `size` bytes, apparent and allocated, on device 1.
pub(crate) fn meta(kind: Kind, size: u64, ino: u64) -> Meta {
    Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino,
        nlink: 1,
        mount: 0,
    }
}
