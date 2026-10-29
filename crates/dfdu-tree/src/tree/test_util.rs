//! Helpers shared by the unit tests of the tree's modules.

use super::{Ix, Tree};
use crate::{Kind, Meta};

pub(super) fn dir_meta() -> Meta {
    Meta {
        kind: Kind::Dir,
        apparent: 10,
        alloc: 4096,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 2,
        mount: 0,
    }
}

pub(super) fn file_meta(size: u64, ino: u64, nlink: u32) -> Meta {
    Meta {
        kind: Kind::File,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino,
        nlink,
        mount: 0,
    }
}

pub(super) fn add_file(t: &mut Tree, p: Ix, name: &str, size: u64) -> Ix {
    let f = t.add_file(p, name.as_bytes(), &file_meta(size, size, 1), false, None);
    let own = t.own(f);
    t.propagate_add(p, &own);
    f
}

pub(super) fn add_dir(t: &mut Tree, p: Ix, name: &str) -> Ix {
    let d = t.add_dir(p, name.as_bytes(), &dir_meta());
    let own = t.own(d);
    t.propagate_add(p, &own);
    d
}
