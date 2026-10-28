//! Native cleanup commands through the engine: the dry run shows its output and changes
//! nothing, the real run is journalled, its output kept, and the matched directory's parent
//! rescanned afterwards.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Model};
use dfdu_rules::{Env, Source, load_from};
use dfdu_scan::JobOptions;
use dfdu_scan::PortableBackend;
use dfdu_scan::classify::ClassOpts;

mod common;

const RULE: &str = r#"schema_version = 1
[[rule]]
id = "test.junk"
category = "dev.build"
name = "Test junk"
safety = "safe"
action = "native"
explanation = "Test."
lost = "Nothing."
[rule.match]
kind = "dir"
names = ["junk"]
[rule.native]
check = ["true"]
dry_run = ["ls", "{path}"]
command = ["rm", "-r", "{name}"]
cwd = "{parent}"
[[rule.native.variant]]
name = "also the sibling"
command = ["rm", "-r", "{name}", "other"]
"#;

/// Waits until `cond` holds of the model, then until the engine is idle.
fn wait_then_idle(e: &Engine<PortableBackend>, cond: impl Fn(&Model) -> bool) {
    common::wait_for(e, cond);
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
}

#[test]
fn dry_run_then_run() {
    // Not under the system temp directory, which is a protected path.
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("p");
    fs::create_dir_all(root.join("junk")).unwrap();
    fs::write(root.join("junk/a.o"), vec![0u8; 9000]).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    let (rules, errors) = load_from(&Env::current(), &[(Source::Builtin, RULE.into())]);
    assert!(errors.is_empty(), "{errors:?}");
    let state = tempfile::tempdir().unwrap();
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            journal_dir: Some(state.path().join("ops")),
            check_invariants: true,
            workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class: Some(ClassOpts::for_root(Arc::new(rules), &root)),
            ..JobOptions::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let junk = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let j = m
            .tree
            .children(r)
            .find(|&c| m.tree.name(c) == b"junk")
            .unwrap();
        assert!(m.tree.class(j).is_some(), "junk is matched");
        m.tree.node_ref(j)
    });

    e.send(Command::RunNative {
        id: 1,
        node: junk,
        variant: None,
        dry_run: true,
    });
    wait_then_idle(&e, |m| {
        m.status.native.as_ref().is_some_and(|n| n.done.is_some())
    });
    let n = e.read(|m| m.status.native.clone()).unwrap();
    assert_eq!(n.done, Some(Ok(())));
    assert!(n.dry_run && n.command.starts_with("ls "), "{n:?}");
    assert_eq!(n.lines.back().map(String::as_str), Some("a.o"), "{n:?}");
    assert!(root.join("junk/a.o").exists());

    e.send(Command::RunNative {
        id: 2,
        node: junk,
        variant: None,
        dry_run: false,
    });
    wait_then_idle(&e, |m| {
        m.status
            .native
            .as_ref()
            .is_some_and(|n| n.id == 2 && n.done.is_some())
    });
    let n = e.read(|m| m.status.native.clone()).unwrap();
    assert_eq!(n.done, Some(Ok(())), "{n:?}");
    assert_eq!(n.command, "rm -r junk");
    assert_eq!(n.cwd.as_deref(), Some(root.as_path()));
    assert!(!root.join("junk").exists() && root.join("other").exists());
    // The rescan of `p` found junk gone.
    wait_then_idle(&e, |m| {
        let r = m.tree.roots().next().unwrap();
        m.tree.children(r).all(|c| m.tree.name(c) != b"junk")
    });
    e.shutdown(Duration::from_secs(5));
    let journal = fs::read_dir(state.path().join("ops"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let kinds: Vec<_> = fs::read_to_string(journal.path())
        .unwrap()
        .lines()
        .map(|l| dfdu_clean::check_line(l).unwrap())
        .collect();
    assert_eq!(kinds, ["session", "native", "native-done"]);
}
