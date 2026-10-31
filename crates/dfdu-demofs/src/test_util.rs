//! A small fixture and helpers the unit tests share.

use crate::{Fixture, Tree};

/// A fixture with one of each kind of entry: duplicates, text, a fill, a symlink, a hard link,
/// a faulty and delayed directory and a sparse file.
pub(crate) const SMALL: &str = r#"
now = "2026-10-01T09:00:00Z"
age = "10d"

[disk]
total = "1TB"
free = "200GB"

[[entry]]
path = "Downloads/a.iso"
size = "1MiB"
content = "iso"
age = "200d"

[[entry]]
path = "Downloads/old/a (1).iso"
size = "1MiB"
content = "iso"

[[entry]]
path = "Downloads/b.iso"
size = "1MiB"

[[entry]]
path = "dev/shop/package.json"
text = "{}"

[[entry]]
path = "dev/shop/node_modules"
fill = { files = 50, size = "10MiB", style = "npm", seed = 3 }

[[entry]]
path = "dev/shop/link"
target = "package.json"

[[entry]]
path = "dev/shop/hard"
link = "dev/shop/package.json"

[[entry]]
path = "Library/locked"
kind = "dir"
error = "denied"
delay = "250ms"

[[entry]]
path = "sparse.img"
size = "10GiB"
alloc = "4KiB"
"#;

/// The tree of [`SMALL`].
pub(crate) fn small() -> Tree {
    Fixture::parse(SMALL).unwrap().build().unwrap()
}

/// Every byte of file `n`.
pub(crate) fn read_all(t: &Tree, n: usize) -> Vec<u8> {
    t.read(n, 0, u32::try_from(t.size(n)).unwrap()).unwrap()
}

/// The names in directory `dir`, in listing order.
pub(crate) fn names(t: &Tree, dir: usize) -> Vec<Vec<u8>> {
    t.list(dir)
        .unwrap()
        .iter()
        .map(|e| e.name.clone())
        .collect()
}
