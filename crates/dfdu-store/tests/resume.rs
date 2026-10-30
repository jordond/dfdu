//! `--resume-from`: a snapshot loaded into the engine is shown at once, and a rescan of its
//! root replaces it with what is on disk now (the shadow-subtree swap).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_store::ncdu::Limits;
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

/// What stat'ing `root` gives, as a resume is sent it after the journal was asked.
fn stat(root: &Path) -> Option<Box<dfdu_tree::Meta>> {
    fs::metadata(root)
        .ok()
        .map(|m| Box::new(dfdu_tree::Meta::from_metadata(&m)))
}

fn root_total(e: &Engine<PortableBackend>) -> Totals {
    e.read(|m| m.tree.total(m.tree.roots().next().unwrap()))
}

fn scan(root: &Path) -> (Vec<u8>, Totals) {
    let e = engine();
    e.send(Command::Scan {
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
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

    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    assert_eq!(root_total(&e), before, "shown as saved");
    e.send(Command::Resume {
        node,
        opts: JobOptions::default(),
        changed: None,
        since: 0,
        root: None,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
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

    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
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
        changed: None,
        since: 0,
        root: None,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
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

/// With the directories the change journal names, a resume classifies the loaded tree and
/// rescans only those: what changed elsewhere stays as saved until it is rescanned itself.
#[test]
fn resume_rescans_only_changed_directories() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    fs::create_dir_all(root.join("web/node_modules/pkg")).unwrap();
    fs::write(root.join("web/package.json"), "{}").unwrap();
    fs::write(root.join("web/node_modules/pkg/index.js"), vec![1u8; 5_000]).unwrap();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/two"), vec![1u8; 9_000]).unwrap();
    fs::create_dir(root.join("quiet")).unwrap();
    let (snap, _) = scan(&root);

    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::write(root.join("a/b/c/new"), vec![1u8; 70_000]).unwrap();
    fs::create_dir_all(root.join("a/b/node_modules/dep")).unwrap();
    fs::write(root.join("a/b/package.json"), "{}").unwrap();
    fs::write(root.join("a/b/node_modules/dep/i.js"), vec![1u8; 3_000]).unwrap();
    fs::write(root.join("quiet/unseen"), vec![1u8; 40_000]).unwrap();

    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    let (rules, errors) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    assert!(errors.is_empty(), "{errors:?}");
    let rules = std::sync::Arc::new(rules);
    e.send(Command::Resume {
        node,
        opts: JobOptions {
            class: Some(dfdu_scan::classify::ClassOpts::for_root(rules, &root)),
            ..JobOptions::default()
        },
        changed: Some(vec![
            "a/b".into(),
            "a/b/c".into(),
            "a/b/node_modules/dep".into(),
        ]),
        since: 0,
        root: stat(&root),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let find = |m: &dfdu_engine::Model, rel: &str| {
        let mut at = m.tree.roots().next().unwrap();
        for n in rel.split('/') {
            at = m
                .tree
                .children(at)
                .find(|&c| m.tree.name(c) == n.as_bytes())?;
        }
        Some(at)
    };
    let classes = |e: &Engine<PortableBackend>| {
        e.read(|m| {
            let rules = m.rules.as_ref().unwrap();
            let mut v: Vec<_> = m
                .tree
                .classes()
                .filter(|(ix, _)| m.tree.is_visible(*ix))
                .map(|(ix, c)| (m.tree.path_buf(ix), rules.rule(c.rule).id.clone()))
                .collect();
            v.sort();
            v
        })
    };
    e.read(|m| {
        assert!(
            find(m, "a/b/c/new").is_some(),
            "a changed directory is rescanned"
        );
        assert!(find(m, "quiet/unseen").is_none(), "an unchanged one is not");
    });
    let nm = "node.node_modules".to_string();
    assert_eq!(
        classes(&e),
        [
            (root.join("a/b/node_modules"), nm.clone()),
            (root.join("web/node_modules"), nm.clone()),
        ]
    );

    // A later rescan of a directory the resume did not touch keeps classifying.
    let quiet = e.read(|m| m.tree.node_ref(find(m, "quiet").unwrap()));
    e.send(Command::Rescan { node: quiet });
    let web = e.read(|m| m.tree.node_ref(find(m, "web").unwrap()));
    e.send(Command::Rescan { node: web });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    e.read(|m| assert!(find(m, "quiet/unseen").is_some()));
    assert_eq!(classes(&e).len(), 2);
    let t = root_total(&e);
    let (_, now) = scan(&root);
    assert_eq!(
        (t.apparent, t.alloc, t.items),
        (now.apparent, now.alloc, now.items)
    );
    e.shutdown(Duration::from_secs(1));
}

/// Classifying a loaded tree gives the classes and protection a scan of the same files gives,
/// marker files (read from disk) included.
#[test]
fn a_loaded_tree_is_classified_as_a_scan_would() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("tree");
    let tag = "Signature: 8a477f597d28d172789f06886806bc55\n";
    for (f, data) in [
        ("cache/CACHEDIR.TAG", tag),
        ("cache/blob", "x"),
        ("fake-cache/CACHEDIR.TAG", "not a tag"),
        ("proj/Cargo.toml", "[package]"),
        ("proj/target/CACHEDIR.TAG", tag),
        ("proj/target/debug/out", "x"),
        ("py/.venv/pyvenv.cfg", "home = /usr"),
        ("py/.venv/lib/x.py", ""),
        ("web/package.json", "{}"),
        ("web/node_modules/m/i.js", ""),
    ] {
        let p = root.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    let (rules, errors) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    assert!(errors.is_empty(), "{errors:?}");
    let rules = std::sync::Arc::new(rules);
    let opts = || JobOptions {
        class: Some(dfdu_scan::classify::ClassOpts::for_root(
            rules.clone(),
            &root,
        )),
        ..JobOptions::default()
    };
    let marks = |e: &Engine<PortableBackend>| {
        e.read(|m| {
            let mut v = Vec::new();
            let mut stack: Vec<_> = m.tree.roots().collect();
            while let Some(ix) = stack.pop() {
                let f = m.tree.flags(ix);
                let class = m.tree.class(ix).map(|c| (c.rule, c.safety, c.degraded));
                if class.is_some() || f.has(dfdu_tree::NodeFlags::PROTECTED) {
                    v.push((m.tree.path_buf(ix), class, m.tree.is_protected(ix)));
                }
                stack.extend(m.tree.children(ix));
            }
            v.sort();
            v
        })
    };
    let e = engine();
    e.send(Command::Scan {
        path: root.clone().into(),
        opts: opts(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let scanned = marks(&e);
    let snap = e.read(|m| {
        let mut out = vec![];
        let r = m.tree.roots().next().unwrap();
        snapshot::write(&m.tree, r, &mut out, &Header::default()).unwrap();
        out
    });
    e.shutdown(Duration::from_secs(1));
    assert_eq!(scanned.len(), 4, "{scanned:?}");

    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    e.send(Command::Resume {
        node,
        opts: opts(),
        changed: Some(vec![]),
        since: 0,
        root: stat(&root),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    assert_eq!(marks(&e), scanned);
    e.shutdown(Duration::from_secs(1));
}

/// How long classifying a large loaded tree takes (`DFDU_TREE=path`, `--ignored`, release).
#[test]
#[ignore = "a timing run on the tree DFDU_TREE names"]
fn classify_loaded_cost() {
    let Some(root) = std::env::var_os("DFDU_TREE").map(std::path::PathBuf::from) else {
        return;
    };
    let root = dunce::canonicalize(root).unwrap();
    let (snap, _) = scan(&root);
    let (rules, _) = dfdu_rules::builtin(&dfdu_rules::Env::current());
    let rules = std::sync::Arc::new(rules);
    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    let t = std::time::Instant::now();
    e.send(Command::Resume {
        node,
        opts: JobOptions {
            class: Some(dfdu_scan::classify::ClassOpts::for_root(rules, &root)),
            ..JobOptions::default()
        },
        changed: Some(vec![]),
        since: 0,
        root: stat(&root),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(600))).is_idle());
    let n = e.read(|m| m.tree.live_nodes());
    eprintln!("{n} nodes classified in {:?}", t.elapsed());
    e.shutdown(Duration::from_secs(1));
}

/// btrfs, as root (`DFDU_BTRFS_DIR`, see `scripts/btrfs-fixture.sh`): the subvolume's
/// generation tells which directories changed since, leaving out those only read (atime
/// updates, the scan's own among them), and a resume rescans those and every subvolume below,
/// whose changes are in a tree of its own.
#[cfg(target_os = "linux")]
#[test]
fn btrfs_generations_tell_what_changed() {
    use std::path::PathBuf;
    use std::process::Command as Proc;
    let Some(f) = std::env::var_os("DFDU_BTRFS_DIR").map(PathBuf::from) else {
        return;
    };
    let btrfs = |args: &[&str]| {
        let st = Proc::new("btrfs").arg("-q").args(args).status().unwrap();
        assert!(st.success(), "{args:?}");
    };
    let root = f.join(format!("work-resume-{}", std::process::id()));
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::create_dir(root.join("quiet")).unwrap();
    fs::write(root.join("quiet/old"), vec![1u8; 5_000]).unwrap();
    fs::write(root.join("a/grow"), vec![1u8; 5_000]).unwrap();
    let sv = root.join("sv");
    btrfs(&["subvolume", "create", sv.to_str().unwrap()]);
    fs::write(sv.join("in"), vec![1u8; 9_000]).unwrap();
    // An atime older than the mtime: reading the file updates it (relatime).
    let st = Proc::new("touch")
        .args(["-a", "-d", "2000-01-01"])
        .arg(root.join("quiet/old"))
        .status()
        .unwrap();
    assert!(st.success());
    // Everything above has a ctime more than a second before the position's time.
    std::thread::sleep(Duration::from_millis(2100));
    let Some(at) = dfdu_engine::journal_position(&root) else {
        // Not root: no position.
        btrfs(&["subvolume", "delete", sv.to_str().unwrap()]);
        fs::remove_dir_all(&root).unwrap();
        return;
    };
    let (snap, _) = scan(&root);

    fs::read(root.join("quiet/old")).unwrap();
    fs::write(root.join("a/b/new"), vec![1u8; 70_000]).unwrap();
    let mut grow = fs::OpenOptions::new()
        .append(true)
        .open(root.join("a/grow"))
        .unwrap();
    std::io::Write::write_all(&mut grow, &[2u8; 40_000]).unwrap();
    drop(grow);
    fs::write(sv.join("added"), vec![1u8; 30_000]).unwrap();

    let changed = dfdu_engine::changed_since(&root, at).expect("can tell");
    let want: Vec<PathBuf> = vec!["a".into(), "a/b".into()];
    assert_eq!(changed, want);
    let mut other = at;
    other.volume[0] ^= 0xff;
    assert_eq!(dfdu_engine::changed_since(&root, other), None);

    let loaded = snapshot::read(&snap[..], &Limits::default()).unwrap();
    let e = engine();
    let node = loaded.tree.node_ref(loaded.root);
    e.load(loaded.tree);
    e.send(Command::Resume {
        node,
        opts: JobOptions::default(),
        changed: Some(changed),
        since: 0,
        root: stat(&root),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let resumed = root_total(&e);
    e.shutdown(Duration::from_secs(1));
    let (_, now) = scan(&root);
    assert_eq!(
        resumed, now,
        "the subvolume and the changed directories were rescanned"
    );

    btrfs(&["subvolume", "delete", sv.to_str().unwrap()]);
    fs::remove_dir_all(&root).unwrap();
}
