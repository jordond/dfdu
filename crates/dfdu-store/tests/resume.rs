//! `--resume-from`: a snapshot loaded into the engine is shown at once, and a rescan of its
//! root replaces it with what is on disk now (the shadow-subtree swap).

use std::fs;
use std::path::Path;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_store::snapshot::{self, Header};
use dfdu_tree::Totals;

fn engine() -> Engine<PortableBackend> {
    Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            check_invariants: true,
            ..Default::default()
        },
    )
}

fn root_total(e: &Engine<PortableBackend>) -> Totals {
    e.read(|m| m.tree.total(m.tree.roots().next().unwrap()))
}

fn scan(root: &Path) -> (Vec<u8>, Totals) {
    let e = engine();
    e.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let out = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let mut out = vec![];
        snapshot::write(&m.tree, r, &mut out, &Header::default()).unwrap();
        (out, m.tree.total(r))
    });
    e.shutdown(Duration::from_secs(1));
    out
}

#[test]
fn resumed_snapshot_is_rescanned() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/one"), vec![1u8; 30_000]).unwrap();
    fs::write(root.join("a/b/two"), vec![1u8; 9_000]).unwrap();
    let (snap, before) = scan(&root);

    fs::remove_file(root.join("a/one")).unwrap();
    fs::create_dir(root.join("new")).unwrap();
    fs::write(root.join("new/three"), vec![1u8; 70_000]).unwrap();
    let (_, now) = scan(&root);
    assert_ne!(before.alloc, now.alloc);

    let loaded = snapshot::read(&snap[..], &Default::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    assert_eq!(root_total(&e), before, "shown as saved");
    e.send(Command::Rescan { node });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    let t = root_total(&e);
    assert_eq!(
        (t.apparent, t.alloc, t.items),
        (now.apparent, now.alloc, now.items)
    );
    e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        assert_eq!(m.tree.path_buf(r), root);
        assert!(m.tree.children(r).any(|c| m.tree.name(c) == b"new"));
    });
    e.shutdown(Duration::from_secs(1));
}

/// A resumed scan classifies with the rules it is given: the saved tree has no classes, and
/// the rescan of its root finds them.
#[test]
fn resumed_snapshot_is_classified() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    fs::create_dir_all(root.join("web/node_modules/pkg")).unwrap();
    fs::write(root.join("web/package.json"), "{}").unwrap();
    fs::write(root.join("web/package-lock.json"), "{}").unwrap();
    fs::write(root.join("web/node_modules/pkg/index.js"), vec![1u8; 5_000]).unwrap();
    let (snap, _) = scan(&root);

    let loaded = snapshot::read(&snap[..], &Default::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    assert_eq!(e.read(|m| m.tree.classes().count()), 0);
    let (rules, errors) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    assert!(errors.is_empty(), "{errors:?}");
    let rules = std::sync::Arc::new(rules);
    e.send(Command::Resume {
        node,
        opts: JobOptions {
            class: Some(dfdu_scan::classify::ClassOpts::for_root(rules, &root)),
            ..JobOptions::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))));
    e.read(|m| {
        let rules = m.rules.as_ref().expect("the rules are the model's");
        let classes: Vec<_> = m
            .tree
            .classes()
            .map(|(ix, c)| (m.tree.path_buf(ix), rules.rule(c.rule).id.clone()))
            .collect();
        assert_eq!(
            classes,
            [(
                root.join("web/node_modules"),
                "node.node_modules".to_string()
            )]
        );
    });
    e.shutdown(Duration::from_secs(1));
}
