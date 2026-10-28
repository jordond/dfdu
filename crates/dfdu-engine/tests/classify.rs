//! Classification during a real scan (architecture §6): the rules the built-in pack decides on
//! the workers reach the tree, including directories wide enough to be read in several blocks,
//! whose markers and siblings are in other blocks (scenario 12 in `data-pipeline.md` §10.2).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Model};
use dfdu_rules::{Env, RuleSet, builtin};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, Limits, PortableBackend};
use dfdu_tree::{Class, Ix, NodeFlags};

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache\n";

fn rules() -> Arc<RuleSet> {
    let (set, errors) = builtin(&Env::current());
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(set)
}

/// A temporary directory in the target directory.
fn tmp() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

fn touch(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"x").unwrap();
}

/// Scans `root` with blocks of `block` entries and returns the model's answer to `f`.
fn scan<T>(root: &Path, block: usize, workers: usize, f: impl FnOnce(&Model) -> T) -> T {
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers,
            journal_dir: None,
            crash_dir: None,
            measure_workers: 0,
            check_invariants: true,
            limits: Limits {
                block_entries: block,
                ..Limits::default()
            },
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: root.to_path_buf(),
        opts: JobOptions {
            class: Some(ClassOpts::for_root(rules(), root)),
            ..JobOptions::default()
        },
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(30))), "scan");
    let out = engine.read(f);
    engine.shutdown(Duration::from_millis(200));
    out
}

fn find(m: &Model, path: &str) -> Ix {
    let mut ix = m.tree.roots().next().expect("root");
    for name in path.split('/').filter(|s| !s.is_empty()) {
        ix = m
            .tree
            .children(ix)
            .find(|&c| m.tree.name(c) == name.as_bytes())
            .unwrap_or_else(|| panic!("{name} of {path}"));
    }
    ix
}

/// The id and safety of the rule node `path` matched itself, if any.
fn class(m: &Model, path: &str) -> Option<String> {
    let c = m.tree.class(find(m, path))?;
    let rules = m.rules.as_ref().expect("rules");
    let safety = ["safe", "regenerable", "review", "manual", "protected"][c.safety as usize];
    Some(format!("{} {safety}", rules.rule(c.rule).id))
}

/// A project tree with one of each kind of match, padded with `pad` files per directory so
/// that small blocks split every listing.
fn project(root: &Path, pad: usize) {
    let p = |s: &str| root.join(s);
    touch(&p("rust/Cargo.toml"));
    touch(&p("rust/target/debug/app"));
    touch(&p("rust/target/debug/node_modules/x"));
    fs::write(p("rust/target/CACHEDIR.TAG"), TAG).unwrap();
    touch(&p("web/package.json"));
    touch(&p("web/node_modules/left-pad/index.js"));
    touch(&p("web/node_modules/left-pad/node_modules/y/z"));
    touch(&p("locked/package.json"));
    touch(&p("locked/yarn.lock"));
    touch(&p("locked/node_modules/a"));
    touch(&p("bare/node_modules/a"));
    touch(&p("py/.venv/pyvenv.cfg"));
    touch(&p("py/requirements.txt"));
    touch(&p("notvenv/.venv/other"));
    fs::create_dir_all(p("cache")).unwrap();
    fs::write(p("cache/CACHEDIR.TAG"), TAG).unwrap();
    touch(&p("cache/sub/dir/f"));
    fs::create_dir_all(p("badtag")).unwrap();
    fs::write(p("badtag/CACHEDIR.TAG"), "not a tag").unwrap();
    for d in ["", "rust", "rust/target", "web", "locked", "py", "cache"] {
        for i in 0..pad {
            touch(&p(format!("{d}/pad{i:04}").trim_start_matches('/')));
        }
    }
}

fn check_project(m: &Model) {
    assert_eq!(
        class(m, "rust/target").as_deref(),
        Some("rust.target regenerable")
    );
    // Inside a matched directory nothing else is classified (`stop_descent`).
    assert_eq!(class(m, "rust/target/debug"), None);
    assert_eq!(class(m, "rust/target/debug/node_modules"), None);
    assert_eq!(
        class(m, "web/node_modules").as_deref(),
        Some("node.node_modules review")
    );
    assert_eq!(class(m, "web/node_modules/left-pad/node_modules"), None);
    assert_eq!(
        class(m, "locked/node_modules").as_deref(),
        Some("node.node_modules regenerable")
    );
    assert_eq!(class(m, "bare/node_modules"), None);
    assert_eq!(
        class(m, "py/.venv").as_deref(),
        Some("python.venv regenerable")
    );
    assert_eq!(class(m, "notvenv/.venv"), None);
    assert_eq!(
        class(m, "cache").as_deref(),
        Some("generic.cachedir-tag safe")
    );
    assert_eq!(class(m, "cache/sub"), None);
    assert_eq!(class(m, "badtag"), None);
    let classified = m.tree.classes().count();
    assert_eq!(classified, 5, "{classified} classified nodes");
    // What a node inside a match counts as.
    let (top, c) = m
        .tree
        .class_in_effect(find(m, "rust/target/debug/app"))
        .expect("in effect");
    assert_eq!(top, find(m, "rust/target"));
    assert_eq!(m.rules.as_ref().unwrap().rule(c.rule).id, "rust.target");
}

#[test]
fn rules_are_decided_during_the_scan() {
    let tmp = tmp();
    project(tmp.path(), 0);
    scan(tmp.path(), 4096, 4, check_project);
}

/// With blocks of 3 entries, every directory above is read in several blocks: `Cargo.toml`,
/// the lockfiles and CACHEDIR.TAG are in other blocks than what they classify, and the blocks
/// are stat'ed by several workers.
#[test]
fn markers_and_siblings_in_other_blocks() {
    let tmp = tmp();
    project(tmp.path(), 7);
    for workers in [1, 2, 8] {
        for block in [1, 2, 3, 5] {
            scan(tmp.path(), block, workers, check_project);
        }
    }
}

#[test]
fn scanning_inside_a_match_starts_from_the_root_s_own_class() {
    let tmp = tmp();
    project(tmp.path(), 0);
    let root = tmp.path().join("rust/target");
    scan(&root, 4096, 2, |m| {
        let r = m.tree.roots().next().unwrap();
        let c = m.tree.class(r).expect("root class");
        assert_eq!(m.rules.as_ref().unwrap().rule(c.rule).id, "rust.target");
        assert_eq!(m.tree.classes().count(), 1);
        assert!(!m.tree.flags(r).has(NodeFlags::PROTECTED));
    });
}

/// `/private/var/folders` is protected on macOS except the user's own temporary directory:
/// a scan root in the user's cache directory there is marked and nothing in it is classified;
/// one in the temporary directory (reached through the `/var` symlink, as `TMPDIR` says) is not.
#[test]
fn a_protected_root_is_marked_and_not_classified() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let out = std::process::Command::new("getconf")
        .arg("DARWIN_USER_CACHE_DIR")
        .output()
        .unwrap();
    let cache = String::from_utf8(out.stdout).unwrap();
    let tmp = tempfile::tempdir_in(cache.trim()).unwrap();
    project(tmp.path(), 0);
    scan(tmp.path(), 4096, 2, |m| {
        let r = m.tree.roots().next().unwrap();
        assert!(m.tree.flags(r).has(NodeFlags::PROTECTED));
        assert!(m.tree.is_protected(find(m, "web/node_modules")));
        assert_eq!(m.tree.classes().count(), 0);
    });
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path(), 0);
    scan(tmp.path(), 4096, 2, |m| {
        let r = m.tree.roots().next().unwrap();
        assert!(!m.tree.is_protected(r));
        assert!(m.tree.classes().count() > 0);
    });
}

/// A node the classifier marked protected is refused before a plan exists.
#[test]
fn a_protected_node_is_not_deleted() {
    let tmp = tmp();
    project(tmp.path(), 0);
    touch(&tmp.path().join("web/.git/HEAD"));
    let jdir = tmp.path().join("journals");
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: Some(jdir),
            ..Default::default()
        },
    );
    let root = tmp.path().join("web");
    engine.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class: Some(ClassOpts::for_root(rules(), &root)),
            ..JobOptions::default()
        },
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(30))), "scan");
    let git = engine.read(|m| {
        let g = find(m, ".git");
        assert!(m.tree.is_protected(g));
        m.tree.node_ref(g)
    });
    engine.send(Command::Delete {
        items: vec![git],
        mode: dfdu_engine::Mode::Permanent,
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(10))));
    engine.read(|m| {
        assert!(m.status.last_delete.is_none());
        assert!(
            m.status.notices.iter().any(|n| n.contains("is protected")),
            "{:?}",
            m.status.notices
        );
    });
    assert!(root.join(".git/HEAD").exists());
    engine.shutdown(Duration::from_millis(200));
}

/// The classifier pool checks a match's guards after its job: a `target` git tracks becomes
/// protected, one it does not is checked and stays as it was.
#[test]
fn a_git_tracked_target_is_protected() {
    let git = |dir: &Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let tmp = tmp();
    if !git(tmp.path(), &["init", "-q"]) {
        return;
    }
    project(tmp.path(), 0);
    touch(&tmp.path().join("vendored/Cargo.toml"));
    touch(&tmp.path().join("vendored/target/release/lib"));
    assert!(git(tmp.path(), &["add", "vendored/target/release/lib"]));
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            journal_dir: None,
            crash_dir: None,
            measure_workers: 0,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: tmp.path().to_path_buf(),
        opts: JobOptions {
            class: Some(ClassOpts::for_root(rules(), tmp.path())),
            ..JobOptions::default()
        },
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(30))), "scan");
    let checked = |m: &Model| {
        m.tree
            .classes()
            .filter(|(_, c)| {
                let g = &m.rules.as_ref().unwrap().rule(c.rule).guard;
                g.not_vcs_tracked || !g.not_running.is_empty()
            })
            .all(|(_, c)| c.has(Class::CHECKED))
    };
    let t0 = std::time::Instant::now();
    while !engine.read(checked) {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "checks did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    engine.read(|m| {
        let vendored = m.tree.class(find(m, "vendored/target")).unwrap();
        assert!(vendored.has(Class::VCS_TRACKED));
        assert_eq!(vendored.safety, dfdu_rules::Safety::Protected as u8);
        let own = m.tree.class(find(m, "rust/target")).unwrap();
        assert!(own.has(Class::CHECKED) && !own.has(Class::VCS_TRACKED));
        assert_eq!(own.safety, dfdu_rules::Safety::Regenerable as u8);
    });
    engine.shutdown(Duration::from_millis(200));
}
