//! What a resume keeps as loaded, and what it reads again although the journal does not name
//! it: a kept directory or the root whose mtime changed, and a kept directory whose loaded
//! class its new listing no longer gives. The journal's answers are given by hand.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dfdu_scan::JobOptions;
use dfdu_scan::classify::ClassOpts;
use dfdu_tree::NodeRef;

mod common;
use common::loaded::{find, loaded, resume, resume_with, scanned, totals, write};
use common::tempdir;

/// Sets the mtime of directory `p` a minute ahead, so that it differs from the one loaded
/// whatever second the test runs in.
#[cfg(unix)]
fn touch_ahead(p: &Path) {
    let later = SystemTime::now() + Duration::from_secs(60);
    fs::File::open(p).unwrap().set_modified(later).unwrap();
}

/// A kept directory whose mtime changed since it was loaded is read again, though the
/// journal named only its parent: its entries changed, whatever the journal said. One whose
/// mtime holds stays as loaded.
#[cfg(unix)]
#[test]
fn a_kept_directory_whose_mtime_changed_is_read_again() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("a/x"), 1_000);
    write(&root.join("quiet/q"), 2_000);
    let tree = loaded(&root);
    let quiet = tree.node_ref(find(&tree, "quiet").unwrap());

    write(&root.join("a/new"), 5_000);
    touch_ahead(&root.join("a"));
    let e = resume(tree, &[""]);
    e.read(|m| {
        assert!(find(&m.tree, "a/new").is_some(), "a is read again");
        assert_eq!(m.tree.resolve_visible(quiet), find(&m.tree, "quiet"));
    });
    let got = totals(&e);
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// The root is listed again when its mtime changed, though the journal named nothing: a
/// change there the journal lost (or never had) still shows. Its subdirectories are kept.
#[cfg(unix)]
#[test]
fn a_root_whose_mtime_changed_is_listed_again() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    write(&root.join("keep/k"), 3_000);
    let tree = loaded(&root);
    let keep = tree.node_ref(find(&tree, "keep").unwrap());

    write(&root.join("top"), 4_000);
    touch_ahead(&root);
    let e = resume(tree, &[]);
    e.read(|m| {
        assert!(find(&m.tree, "top").is_some(), "the root is listed again");
        assert_eq!(m.tree.resolve_visible(keep), find(&m.tree, "keep"));
    });
    let got = totals(&e);
    e.shutdown(Duration::from_secs(1));
    assert_eq!(got, scanned(&root));
}

/// A kept directory keeps the class it was loaded with while its new listing still gives
/// that rule (a Cargo `target/` beside its `Cargo.toml`, a directory with a CACHEDIR.TAG,
/// which only its own entries decide), and is classified again when it does not: a `target/`
/// whose `Cargo.toml` is gone is no longer build output.
#[test]
fn a_kept_directory_is_classified_as_its_new_listing_says() {
    let dir = tempdir();
    let root = dir.path().join("tree");
    let tag = b"Signature: 8a477f597d28d172789f06886806bc55\n";
    write(&root.join("Cargo.toml"), 10);
    write(&root.join("target/debug/x"), 1_000);
    write(&root.join("proj/Cargo.toml"), 10);
    write(&root.join("proj/target/debug/y"), 1_000);
    fs::create_dir(root.join("cache")).unwrap();
    fs::write(root.join("cache/CACHEDIR.TAG"), tag).unwrap();
    let tree = loaded(&root);
    let at = |p: &str| tree.node_ref(find(&tree, p).unwrap());
    let (target, cache) = (at("target"), at("cache"));

    fs::remove_file(root.join("proj/Cargo.toml")).unwrap();
    let (rules, _) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    let rule = |id: &str| rules.index(id).unwrap();
    let (rust, tagged) = (rule("rust.target"), rule("generic.cachedir-tag"));
    let opts = JobOptions {
        class: Some(ClassOpts::for_root(Arc::new(rules.clone()), &root)),
        ..JobOptions::default()
    };
    let e = resume_with(tree, opts, &["", "proj"]);
    e.read(|m| {
        let t = &m.tree;
        let class = |p: &str| t.class(find(t, p).unwrap()).map(|c| c.rule);
        let kept = |r: NodeRef, p: &str| t.resolve_visible(r) == find(t, p);
        assert_eq!(class("target"), Some(rust));
        assert!(kept(target, "target"), "target is kept as loaded");
        assert_eq!(class("cache"), Some(tagged));
        assert!(kept(cache, "cache"), "cache is kept as loaded");
        assert_eq!(class("proj/target"), None, "{:?}", m.status.notices);
    });
    e.shutdown(Duration::from_secs(1));
}
