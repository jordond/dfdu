//! A tree read from a file: browsed, never acted on, even where its paths exist here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_tree::{DirState, Kind, Meta, NodeRef, Tree};

mod common;
use common::tempdir;

/// What `path` is now, as a scan would have recorded it.
fn meta_now(path: &Path) -> Meta {
    let md = fs::symlink_metadata(path).unwrap();
    #[cfg(unix)]
    let (dev, ino, alloc, mtime) = {
        use std::os::unix::fs::MetadataExt;
        (md.dev(), md.ino(), md.blocks() * 512, md.mtime())
    };
    #[cfg(not(unix))]
    let (dev, ino, alloc, mtime) = (0, 0, md.len(), 0);
    Meta {
        kind: if md.is_dir() { Kind::Dir } else { Kind::File },
        apparent: md.len(),
        alloc,
        mtime,
        dev,
        ino,
        nlink: 1,
        mount: 0,
    }
}

/// A complete tree of `root` and its file `name`, true to the filesystem.
fn loaded_tree(root: &Path, name: &str) -> (Tree, NodeRef) {
    let mut t = Tree::default();
    let r = t.new_root(
        &dfdu_tree::name_bytes(root.as_os_str()),
        &meta_now(root),
        true,
    );
    let f = t.add_file(r, name.as_bytes(), &meta_now(&root.join(name)), true, None);
    let own = t.own(f);
    t.propagate_add(r, &own);
    let d = t.dir_mut(r).unwrap();
    d.state = DirState::Complete;
    d.outcome = d.state;
    d.pending = 0;
    d.listing_closed = true;
    d.reported = true;
    let node = t.node_ref(f);
    (t, node)
}

#[test]
fn imported_trees_cannot_delete() {
    let dir = tempdir();
    let root = dir.path();
    fs::write(root.join("f.bin"), [7u8; 3000]).unwrap();
    let journal = tempdir();
    let opts = || EngineOptions {
        journal_dir: Some(journal.path().to_path_buf()),
        workers: 1,
        ..Default::default()
    };

    let (tree, f) = loaded_tree(root, "f.bin");
    let e = Engine::start_imported(tree, opts());
    e.send(Command::Delete {
        items: vec![f],
        mode: Mode::Permanent,
    });
    for dry_run in [false, true] {
        e.send(Command::RunNative {
            id: 1,
            node: f,
            variant: None,
            dry_run,
        });
    }
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    e.read(|m| {
        assert!(m.status.last_delete.is_none());
        let refused = m
            .status
            .notices
            .iter()
            .filter(|n| n.contains("imported scan is read-only"));
        assert_eq!(refused.count(), 3, "{:?}", m.status.notices);
    });
    assert!(root.join("f.bin").exists());
    e.shutdown(Duration::from_secs(1));

    // The same tree loaded to be resumed is deleted from once the resume has taken it over,
    // so what kept the file above was the import.
    #[cfg(unix)]
    {
        let (tree, f) = loaded_tree(root, "f.bin");
        let e = Engine::start(dfdu_scan::PortableBackend::default(), opts());
        let node = tree.node_ref(tree.roots().next().unwrap());
        e.load(tree);
        e.send(Command::Resume {
            node,
            opts: dfdu_scan::JobOptions::default(),
            changed: Some(vec![]),
            since: 0,
        });
        assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
        e.send(Command::Delete {
            items: vec![f],
            mode: Mode::Permanent,
        });
        assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
        let s = e.read(|m| m.status.last_delete.clone()).unwrap();
        assert_eq!((s.deleted, s.refused), (1, 0), "{s:?}");
        assert!(!root.join("f.bin").exists());
        e.shutdown(Duration::from_secs(1));
    }
}

/// A file does not say which host its paths are on: the root's form decides, so another
/// system's export shows its own paths.
#[test]
fn an_imported_trees_host_follows_its_root() {
    use dfdu_tree::Host;
    let dir = Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 1,
        mount: 0,
    };
    for (root, host) in [
        (&br"C:\Users\u"[..], Host::Windows),
        (b"/home/u", Host::Unix),
    ] {
        let mut t = Tree::default();
        let r = t.new_root(root, &dir, true);
        t.add_dir(r, b"proj", &dir);
        let e = Engine::start_imported(t, EngineOptions::default());
        let (got, path) = e.read(|m| {
            let r = m.tree.roots().next().unwrap();
            let p = m.tree.children(r).next().unwrap();
            (m.tree.host(), m.tree.host_path(p))
        });
        assert_eq!(got, host);
        assert_eq!(path.parent().unwrap().as_bytes(), root);
        assert_eq!(path.file_name(), Some(&b"proj"[..]));
        e.shutdown(Duration::from_secs(1));
    }
}
