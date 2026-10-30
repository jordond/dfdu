//! The fixture mounted: what `stat`, `read`, `rm -r`, `rename` and `statvfs` see through the
//! kernel matches the tree.
//!
//! Needs `/dev/fuse` and root (or `fusermount3`); skipped with a message otherwise, unless
//! `DFDU_FUSE_REQUIRED` is set (CI).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

use dfdu_demofs::Fixture;

mod change;
mod read;

const FIXTURE: &str = r#"
now = "2026-10-01T09:00:00Z"

[disk]
total = "100GB"
free = "10GB"
device = "/dev/nvme0n1p2"

[[entry]]
path = "Downloads/a.iso"
size = "3MiB"
content = "iso"
age = "200d"

[[entry]]
path = "Downloads/old/a (1).iso"
size = "3MiB"
content = "iso"

[[entry]]
path = "dev/shop/package.json"
text = "{}"

[[entry]]
path = "dev/shop/node_modules"
fill = { files = 300, size = "40MiB", style = "npm" }

[[entry]]
path = "dev/shop/hard"
link = "dev/shop/package.json"

[[entry]]
path = "dev/shop/current"
target = "package.json"

[[entry]]
path = "Library/locked"
kind = "dir"
error = "denied"

[[entry]]
path = "Library/broken.db"
size = 100
error = "io"

[[entry]]
path = "many"
fill = { files = 2000, size = "2MiB" }

[[entry]]
path = "slow"
kind = "dir"
delay = "300ms"
"#;

/// The fixture mounted at `root`, in a directory of its own; unmounted and removed when
/// dropped, in that order.
struct Mounted {
    root: PathBuf,
    _session: fuser::BackgroundSession,
    _dir: tempfile::TempDir,
}

/// Mounts the fixture, or prints why not and returns `None` (no `/dev/fuse`, no privilege)
/// unless `DFDU_FUSE_REQUIRED` is set.
fn mounted() -> Option<Mounted> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home");
    fs::create_dir(&root).unwrap();
    let f = Fixture::parse(FIXTURE).unwrap();
    match dfdu_demofs::mount(&f, &root) {
        Ok(session) => Some(Mounted {
            root,
            _session: session,
            _dir: dir,
        }),
        Err(e) if std::env::var_os("DFDU_FUSE_REQUIRED").is_none() => {
            eprintln!("skipped: cannot mount a FUSE filesystem ({e:#}); needs /dev/fuse and root");
            None
        }
        Err(e) => panic!("cannot mount a FUSE filesystem: {e:#}"),
    }
}
