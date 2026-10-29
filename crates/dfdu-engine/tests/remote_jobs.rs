//! Jobs over the wire protocol: deletions, extent measurements and native commands run on an
//! agent in a thread at the other end of two pipes, and links lost while they run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, RemoteIo, RemoteRules};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, PortableBackend};

mod common;
use common::node;
use common::remote::{opts, remote, remote_with};
#[cfg(unix)]
use common::wait_for;

/// Deletes through the agent: verified and journalled on its side, the tree updated here, and
/// a staged item put back by an undo.
#[test]
fn deletes_run_on_the_agent() {
    use dfdu_engine::Mode;
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("t");
    let put = |p: &str, n: usize| {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; n]).unwrap();
    };
    put("gone/a", 4000);
    put("gone/sub/b", 4000);
    put("staged/c", 3000);
    put("changed", 100);
    put("kept", 100);
    let ops = base.join("state/ops");
    let (e, agent, _) = remote_with(None, Some(ops.clone()));
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let delete = |items, mode| {
        e.send(Command::Delete { items, mode });
        assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
        e.read(|m| m.status.last_delete.clone())
            .expect("a plan finished")
    };

    fs::write(root.join("changed"), vec![2u8; 300]).unwrap();
    let sum = delete(vec![node(&e, "gone"), node(&e, "changed")], Mode::Permanent);
    assert_eq!((sum.deleted, sum.stale), (1, 1), "{:?}", sum.problems);
    assert!(!root.join("gone").exists());
    assert!(root.join("changed").exists());
    let names = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let mut v: Vec<_> = m
            .tree
            .children(r)
            .map(|c| m.tree.name(c).to_vec())
            .collect();
        v.sort();
        v
    });
    assert_eq!(names, [&b"changed"[..], b"kept", b"staged"]);
    // The agent wrote the journal.
    assert!(fs::read_dir(&ops).unwrap().count() > 0);

    let sum = delete(vec![node(&e, "staged")], Mode::Stage);
    assert_eq!(sum.deleted, 1, "{:?}", sum.problems);
    assert!(!root.join("staged").exists());
    let plan = e.read(|m| m.status.undo.as_ref().map(|u| u.plan)).unwrap();
    e.send(Command::Undo { plan });
    let t = std::time::Instant::now();
    while e.read(|m| m.status.undoing.is_some() || m.status.undo.is_some()) {
        assert!(t.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    assert_eq!(fs::read(root.join("staged/c")).unwrap().len(), 3000);
    node(&e, "staged/c");

    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
}

/// A read-only remote engine sends no plan.
#[test]
fn a_read_only_remote_engine_deletes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("t");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("f"), b"x").unwrap();
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let ops = base.join("ops");
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(PortableBackend::default(), 1, Some(ops), down_r, up_w)
    });
    let (e, _) = Engine::start_remote(
        PortableBackend::default(),
        EngineOptions {
            read_only: true,
            ..opts()
        },
        RemoteIo {
            reader: Box::new(up_r),
            writer: Box::new(down_w),
            child: None,
        },
        None,
    )
    .unwrap();
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    e.send(Command::Delete {
        items: vec![node(&e, "f")],
        mode: dfdu_engine::Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    assert!(root.join("f").exists());
    assert!(!base.join("ops").exists());
    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
}

/// A link that ends while a plan runs answers its items as failed, as nobody knows whether
/// the agent deleted them, and the plan ends.
/// An engine talking to an agent thread (journalling in `ops`) through relays that end the
/// link when the engine sends a request for the agent's deleter, before passing it on. The
/// first handle ends when the link does.
fn cut_at_delete(
    ops: std::path::PathBuf,
    rules: Option<RemoteRules>,
) -> (
    Engine<PortableBackend>,
    Option<ClassOpts>,
    std::thread::JoinHandle<()>,
    std::thread::JoinHandle<std::io::Result<()>>,
) {
    use dfdu_engine::wire::{read_frame, write_frame};
    use std::sync::{Arc, Mutex};
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (agent_in_r, agent_in_w) = std::io::pipe().unwrap();
    let (agent_out_r, agent_out_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(
            PortableBackend::default(),
            1,
            Some(ops),
            agent_in_r,
            agent_out_w,
        )
    });
    let up = Arc::new(Mutex::new(Some(up_w)));
    let up2 = up.clone();
    // Agent to engine, until the other relay hangs up.
    std::thread::spawn(move || {
        let mut r = agent_out_r;
        let mut buf = Vec::new();
        while let Ok(true) = read_frame(&mut r, &mut buf) {
            let mut g = up2.lock().unwrap();
            let Some(w) = g.as_mut() else { return };
            if write_frame(w, &buf).is_err() {
                return;
            }
        }
    });
    // Engine to agent; a delete request is not passed on, and the link ends there.
    let relay = std::thread::spawn(move || {
        let (mut r, mut w) = (down_r, agent_in_w);
        let mut buf = Vec::new();
        while let Ok(true) = read_frame(&mut r, &mut buf) {
            // The tag alone: decoding a native command would need the rules.
            if buf.first() == Some(&6) {
                up.lock().unwrap().take();
                return;
            }
            write_frame(&mut w, &buf).unwrap();
        }
    });
    let (e, c) = Engine::start_remote(
        PortableBackend::default(),
        opts(),
        RemoteIo {
            reader: Box::new(up_r),
            writer: Box::new(down_w),
            child: None,
        },
        rules,
    )
    .unwrap();
    (e, c.class, relay, agent)
}

#[test]
fn a_link_lost_during_a_delete_ends_the_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("t");
    fs::create_dir_all(root.join("d")).unwrap();
    fs::write(root.join("d/f"), b"x").unwrap();
    let (e, _, relay, agent) = cut_at_delete(base.join("ops"), None);
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    e.send(Command::Delete {
        items: vec![node(&e, "d")],
        mode: dfdu_engine::Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    relay.join().unwrap();
    let s = e
        .read(|m| m.status.last_delete.clone())
        .expect("the plan ended");
    assert_eq!((s.deleted, s.failed), (0, 1), "{:?}", s.problems);
    assert!(
        s.problems[0].1.contains("link to the agent ended"),
        "{:?}",
        s.problems
    );
    assert!(e.read(|m| m.status.disconnected.is_some()));
    // The item is still in the tree, no longer marked as being deleted.
    node(&e, "d");
    assert!(root.join("d/f").exists());
    e.shutdown(Duration::from_secs(1));
    let _ = agent.join().unwrap();
}

/// Copies `from` to `to` sharing its data where the filesystem can (an APFS clone, a reflink).
fn clone_file(from: &std::path::Path, to: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let flag = "-c";
    #[cfg(not(target_os = "macos"))]
    let flag = "--reflink=auto";
    let st = std::process::Command::new("cp")
        .arg(flag)
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(st.success());
}

/// A tree of clones of 4 MB files below the temporary directory, and whether its filesystem
/// shares extents (otherwise nothing is measured).
fn clones() -> (tempfile::TempDir, std::path::PathBuf, bool) {
    let tmp = common::tempdir();
    let root = tmp.path().join("t");
    let big: Vec<u8> = (0..4_000_000u32)
        .map(|i| (i * 7 + i / 991).to_le_bytes()[0])
        .collect();
    for d in ["x", "y", "z"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("x/a"), &big).unwrap();
    clone_file(&root.join("x/a"), &root.join("x/b"));
    clone_file(&root.join("x/a"), &root.join("y/c"));
    for i in 0..20 {
        fs::write(root.join(format!("z/{i}")), &big[i * 1000..]).unwrap();
    }
    let shares = dfdu_sys::shares_extents(tmp.path()).unwrap_or(false);
    (tmp, root, shares)
}

fn allocs(e: &Engine<PortableBackend>) -> Vec<(std::path::PathBuf, u64)> {
    e.read(|m| {
        let mut out = Vec::new();
        let mut stack: Vec<_> = m.tree.roots().collect();
        while let Some(ix) = stack.pop() {
            out.push((m.tree.path_buf(ix), m.tree.total(ix).alloc));
            stack.extend(m.tree.children(ix));
        }
        out.sort();
        out
    })
}

/// Shared extents are measured on the agent: clones count once, as in a local scan.
#[test]
fn extents_are_measured_on_the_agent() {
    let (_tmp, root, shares) = clones();
    let local = Engine::start(PortableBackend::default(), opts());
    local.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(local.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let want = allocs(&local);
    let measured = local.read(|m| m.tree.total(m.tree.roots().next().unwrap()).alloc);
    local.shutdown(Duration::from_secs(1));

    let (e, agent) = remote();
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    assert_eq!(allocs(&e), want);
    assert_eq!(e.read(|m| m.status.unmeasured), 0);
    e.shutdown(Duration::from_secs(1));
    let _ = agent.join().unwrap();

    // Without measurement the clones count once each.
    let unmeasured = {
        let e = Engine::start(
            PortableBackend::default(),
            EngineOptions {
                measure_workers: 0,
                ..opts()
            },
        );
        e.send(Command::Scan {
            path: root.clone(),
            opts: JobOptions::default(),
        });
        assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
        let t = e.read(|m| m.tree.total(m.tree.roots().next().unwrap()).alloc);
        e.shutdown(Duration::from_secs(1));
        t
    };
    if shares {
        assert!(measured < unmeasured, "{measured} {unmeasured}");
    } else {
        eprintln!("not on a filesystem sharing extents: nothing measured");
        assert_eq!(measured, unmeasured);
    }
}

/// Files sent to be measured when the link ends count as unmeasured at once, not after the
/// measurement watchdog's minute.
#[test]
fn a_link_lost_while_measuring_leaves_files_unmeasured() {
    use dfdu_engine::wire::{FromAgent, read_frame, write_frame};
    let (_tmp, root, shares) = clones();
    if !shares {
        eprintln!("skipped: not on a filesystem sharing extents");
        return;
    }
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (agent_r, agent_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(PortableBackend::default(), 2, None, down_r, agent_w)
    });
    // Passes the agent's frames on until its first measurement, then hangs up.
    let relay = std::thread::spawn(move || {
        let (mut r, mut w) = (agent_r, up_w);
        let mut buf = Vec::new();
        while read_frame(&mut r, &mut buf).unwrap() {
            if let Ok(FromAgent::Measured(_)) = FromAgent::decode(&buf, None) {
                break;
            }
            write_frame(&mut w, &buf).unwrap();
        }
    });
    let (e, _) = Engine::start_remote(
        PortableBackend::default(),
        opts(),
        RemoteIo {
            reader: Box::new(up_r),
            writer: Box::new(down_w),
            child: None,
        },
        None,
    )
    .unwrap();
    let t = std::time::Instant::now();
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    assert!(t.elapsed() < Duration::from_secs(20));
    relay.join().unwrap();
    let (unmeasured, measuring) = e.read(|m| (m.status.unmeasured, m.status.measuring.clone()));
    assert!(unmeasured > 0);
    assert!(measuring.is_none(), "{measuring:?}");
    assert!(e.read(|m| m.status.disconnected.is_some()));
    e.shutdown(Duration::from_secs(1));
    let _ = agent.join().unwrap();
}

/// A rule with a native command, a dry run, a variant and a size command, from a user rule
/// file: the agent builds each from its own copy of the rules.
#[cfg(unix)]
const NATIVE_RULE: &str = r#"schema_version = 1
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
size_command = ["echo", "12345"]
dry_run = ["ls", "{path}"]
command = ["rm", "-r", "{name}"]
cwd = "{parent}"
[[rule.native.variant]]
name = "slowly"
command = ["sleep", "30"]
"#;

#[cfg(unix)]
fn native_rules(root: &std::path::Path) -> RemoteRules {
    let (mut files, errors) = dfdu_rules::sources(None);
    assert!(errors.is_empty(), "{errors:?}");
    files.push((
        dfdu_rules::Source::File("/rules/junk.toml".into()),
        NATIVE_RULE.into(),
    ));
    RemoteRules {
        files,
        root: root.to_path_buf(),
    }
}

/// Native commands of the rules run on the agent's host: the dry run's output comes back, the
/// size command reports, and the real run changes the agent's disk, is journalled there, and
/// is followed by a rescan.
#[cfg(unix)]
#[test]
fn native_commands_run_on_the_agent() {
    // Not under the system temp directory, which is a protected path.
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("p");
    fs::create_dir_all(root.join("junk")).unwrap();
    fs::write(root.join("junk/a.o"), vec![0u8; 9000]).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    let ops = base.join("state/ops");
    let (e, agent, class) = remote_with(Some(native_rules(&root)), Some(ops.clone()));
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class,
            ..JobOptions::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let junk = node(&e, "junk");
    let rule = e.read(|m| {
        let ix = m.tree.resolve_visible(junk).unwrap();
        m.tree.class(ix).expect("junk is matched").rule
    });
    let done = |id: u64| {
        move |m: &dfdu_engine::Model| {
            m.status
                .native
                .as_ref()
                .is_some_and(|n| n.id == id && n.done.is_some())
        }
    };

    e.send(Command::RunNative {
        id: 1,
        node: junk,
        variant: None,
        dry_run: true,
    });
    wait_for(&e, done(1));
    let n = e.read(|m| m.status.native.clone()).unwrap();
    assert_eq!(n.done, Some(Ok(())), "{n:?}");
    assert_eq!(n.lines.back().map(String::as_str), Some("a.o"), "{n:?}");
    assert!(root.join("junk/a.o").exists());

    e.send(Command::NativeSizes);
    wait_for(&e, |m| m.native_sizes.contains_key(&rule));
    assert_eq!(e.read(|m| m.native_sizes[&rule].clone()), Ok(12345));

    e.send(Command::RunNative {
        id: 2,
        node: junk,
        variant: None,
        dry_run: false,
    });
    wait_for(&e, done(2));
    let n = e.read(|m| m.status.native.clone()).unwrap();
    assert_eq!(n.done, Some(Ok(())), "{n:?}");
    assert_eq!(n.command, "rm -r junk");
    assert!(!root.join("junk").exists() && root.join("other").exists());
    wait_for(&e, |m| {
        let r = m.tree.roots().next().unwrap();
        m.tree.children(r).all(|c| m.tree.name(c) != b"junk")
    });
    // Journalled on the agent's side.
    assert!(fs::read_dir(&ops).unwrap().count() > 0);
    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
}

/// A native command running when the link ends is answered at once: it may have run.
#[cfg(unix)]
#[test]
fn a_link_lost_during_a_native_command_ends_it() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("p");
    fs::create_dir_all(root.join("junk")).unwrap();
    let (e, class, relay, agent) = cut_at_delete(base.join("ops"), Some(native_rules(&root)));
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class,
            ..JobOptions::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    e.send(Command::RunNative {
        id: 7,
        node: node(&e, "junk"),
        variant: Some(0),
        dry_run: false,
    });
    relay.join().unwrap();
    wait_for(&e, |m| {
        m.status.native.as_ref().is_some_and(|n| n.done.is_some())
    });
    let n = e.read(|m| m.status.native.clone()).unwrap();
    assert!(
        n.done
            .as_ref()
            .unwrap()
            .as_ref()
            .is_err_and(|e| e.contains("link to the agent ended")),
        "{n:?}"
    );
    assert!(root.join("junk").exists());
    e.shutdown(Duration::from_secs(1));
    let _ = agent.join().unwrap();
}
