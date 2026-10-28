//! Remote scans over the wire protocol: an agent in a thread at the other end of two pipes
//! scans a directory, and the local engine builds the same tree a local scan builds.

use std::fs;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, RemoteIo, RemoteRules};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, PortableBackend};

fn opts() -> EngineOptions {
    EngineOptions {
        workers: 2,
        journal_dir: None,
        check_invariants: true,
        ..Default::default()
    }
}

/// An engine talking to an agent thread; the agent ends when the engine closes its stdin.
fn remote() -> (
    Engine<PortableBackend>,
    std::thread::JoinHandle<std::io::Result<()>>,
) {
    let (e, agent, _) = remote_with(None, None);
    (e, agent)
}

/// [`remote`], classifying with `rules`, the agent journalling deletions in `journal`; also
/// what the root classifies against.
fn remote_with(
    rules: Option<RemoteRules>,
    journal: Option<std::path::PathBuf>,
) -> (
    Engine<PortableBackend>,
    std::thread::JoinHandle<std::io::Result<()>>,
    Option<ClassOpts>,
) {
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(PortableBackend::default(), 2, journal, down_r, up_w)
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
    assert!(c.tool.starts_with("dfdu "), "{}", c.tool);
    assert_eq!(c.errors, Vec::<String>::new());
    (e, agent, c.class)
}

fn shape(e: &Engine<PortableBackend>) -> Vec<(std::path::PathBuf, u64, u64)> {
    e.read(|m| {
        let mut out = Vec::new();
        let mut stack: Vec<_> = m.tree.roots().collect();
        while let Some(ix) = stack.pop() {
            let t = m.tree.total(ix);
            out.push((m.tree.path_buf(ix), t.apparent, t.items));
            stack.extend(m.tree.children(ix));
        }
        out.sort();
        out
    })
}

#[test]
fn a_remote_scan_builds_the_local_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("t");
    for i in 0..30 {
        let d = root.join(format!("d{}/e{}", i % 4, i % 7));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("f{i}")), vec![1u8; i * 100]).unwrap();
    }
    // Wide enough for several chunks.
    fs::create_dir_all(root.join("wide")).unwrap();
    for i in 0..10_000 {
        fs::write(root.join(format!("wide/{i}")), b"").unwrap();
    }

    let local_scan = || {
        let local = Engine::start(PortableBackend::default(), opts());
        local.send(Command::Scan {
            path: root.clone(),
            opts: JobOptions::default(),
        });
        assert!(local.wait_idle(Some(Duration::from_secs(30))).is_idle());
        let s = shape(&local);
        local.shutdown(Duration::from_secs(1));
        s
    };
    let want = local_scan();

    let (e, agent) = remote();
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    assert_eq!(e.read(|m| m.status.fatal.clone()), None);
    assert_eq!(shape(&e), want);

    // A rescan of a subdirectory goes to the agent too.
    fs::write(root.join("d1/new"), vec![0u8; 5000]).unwrap();
    let node = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        let d1 = m
            .tree
            .children(r)
            .find(|&c| m.tree.name(c) == b"d1")
            .unwrap();
        m.tree.node_ref(d1)
    });
    e.send(Command::Rescan { node });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    assert_eq!(shape(&e), local_scan());

    // Without a journal on the agent, nothing is deleted.
    e.send(Command::Delete {
        items: vec![node],
        mode: dfdu_engine::Mode::Permanent,
    });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    assert!(root.join("d1/new").exists());

    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
}

/// An agent that goes away before sending anything ends the scan: nothing was scanned.
#[test]
fn a_closed_link_is_reported() {
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let agent = std::thread::spawn(move || {
        // Says hello, reads the greeting, then hangs up.
        let mut up_w = up_w;
        let mut down_r = down_r;
        let hello = dfdu_engine::wire::FromAgent::Hello {
            min: dfdu_engine::wire::VERSION,
            max: dfdu_engine::wire::VERSION,
            tool: "fake".into(),
        };
        dfdu_engine::wire::write_frame(&mut up_w, &hello.encode()).unwrap();
        let host = dfdu_engine::wire::FromAgent::Host {
            measure_min: 0,
            privileged: false,
        };
        dfdu_engine::wire::write_frame(&mut up_w, &host.encode()).unwrap();
        let mut buf = Vec::new();
        dfdu_engine::wire::read_frame(&mut down_r, &mut buf).unwrap();
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
    agent.join().unwrap();
    e.send(Command::Scan {
        path: "/nowhere".into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(10))).is_idle());
    let why = e.read(|m| m.status.disconnected.clone()).unwrap();
    assert!(why.contains("closed the link"), "{why}");
    assert_eq!(e.read(|m| m.status.fatal.clone()), None);
    e.shutdown(Duration::from_secs(1));
}

/// An agent that says nothing for longer than `DEAD` is taken for gone: its process is
/// killed and the scan ends with an error.
#[cfg(unix)]
#[test]
fn a_silent_agent_is_given_up() {
    use std::process::{Command as Proc, Stdio};
    let tmp = tempfile::tempdir().unwrap();
    let hello = tmp.path().join("hello");
    let mut frame = Vec::new();
    let up = dfdu_engine::wire::FromAgent::Hello {
        min: dfdu_engine::wire::VERSION,
        max: dfdu_engine::wire::VERSION,
        tool: "silent".into(),
    };
    dfdu_engine::wire::write_frame(&mut frame, &up.encode()).unwrap();
    let host = dfdu_engine::wire::FromAgent::Host {
        measure_min: 0,
        privileged: false,
    };
    dfdu_engine::wire::write_frame(&mut frame, &host.encode()).unwrap();
    fs::write(&hello, frame).unwrap();
    let mut child = Proc::new("sh")
        .arg("-c")
        .arg(format!("cat '{}'; exec sleep 60", hello.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let io = RemoteIo {
        reader: Box::new(child.stdout.take().unwrap()),
        writer: Box::new(child.stdin.take().unwrap()),
        child: Some(child),
    };
    let (e, c) = Engine::start_remote(PortableBackend::default(), opts(), io, None).unwrap();
    assert_eq!(c.tool, "silent");
    let t = std::time::Instant::now();
    e.send(Command::Scan {
        path: "/".into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let why = e.read(|m| m.status.disconnected.clone()).unwrap();
    assert!(why.contains("no data for 10 s"), "{why}");
    assert!(e.read(|m| m.status.incomplete));
    assert!(t.elapsed() >= dfdu_engine::DEAD, "{:?}", t.elapsed());
    e.shutdown(Duration::from_secs(1));
}

/// A node's path, its rule's id, safety (`None` for a protected node), degrade condition and
/// flags.
type ClassRow = (
    std::path::PathBuf,
    String,
    Option<dfdu_rules::Safety>,
    u8,
    u8,
);

/// Every class in the tree once its guard checks ran, and protection.
fn classes(e: &Engine<PortableBackend>) -> Vec<ClassRow> {
    let t = std::time::Instant::now();
    while e.read(|m| {
        m.tree
            .classes()
            .any(|(_, c)| c.has(dfdu_tree::Class::QUEUED))
    }) {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "guard checks never answered"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    e.read(|m| {
        let rules = m.rules.clone().expect("rules");
        let mut out: Vec<_> = m
            .tree
            .classes()
            .map(|(ix, c)| {
                (
                    m.tree.path_buf(ix),
                    rules.rule(c.rule).id.clone(),
                    Some(c.safety),
                    c.degraded.map_or(0, |d| d as u8 + 1),
                    c.flags,
                )
            })
            .collect();
        let mut stack: Vec<_> = m.tree.roots().collect();
        while let Some(ix) = stack.pop() {
            if m.tree.is_protected(ix) {
                out.push((m.tree.path_buf(ix), "protected".into(), None, 0, 0));
            }
            stack.extend(m.tree.children(ix));
        }
        out.sort();
        out
    })
}

/// With the rules sent along, the agent classifies as a local scan does, rescans included.
#[test]
fn a_remote_scan_classifies_as_a_local_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("t");
    let w = |p: &str, b: &[u8]| {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b).unwrap();
    };
    // A lockfile next to one node_modules, none next to the other (degraded).
    w("web/package.json", b"{}");
    w("web/package-lock.json", b"{}");
    w("web/node_modules/x/index.js", &[1; 3000]);
    w("old/package.json", b"{}");
    w("old/node_modules/y/index.js", &[1; 3000]);
    // Decided inside: a venv by its pyvenv.cfg, a cache by CACHEDIR.TAG.
    w("py/.venv/pyvenv.cfg", b"home = /usr/bin");
    w("py/.venv/lib/a.py", &[2; 900]);
    w(
        "c/cache/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55",
    );
    w("rs/Cargo.toml", b"[package]");
    w("rs/target/debug/app", &[3; 5000]);
    for i in 0..3000 {
        w(&format!("wide/f{i}"), b"");
    }
    w("wide/node_modules/z.js", b"");
    w("wide/package.json", b"{}");
    // Tracked by git: the guard check, run where the files are, protects it.
    w("git/package.json", b"{}");
    w("git/package-lock.json", b"{}");
    w("git/node_modules/t.js", b"tracked");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root.join("git"))
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "node_modules/t.js"]);

    let env = dfdu_rules::Env::current();
    let (files, _) = dfdu_rules::sources(None);
    let rules = std::sync::Arc::new(dfdu_rules::load_from(&env, &files).0);
    let local = Engine::start(PortableBackend::default(), opts());
    local.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class: Some(ClassOpts::for_root(rules, &root)),
            ..JobOptions::default()
        },
    });
    assert!(local.wait_idle(Some(Duration::from_secs(30))).is_idle());
    let want = classes(&local);
    assert!(want.len() >= 5, "{want:?}");
    let tracked = dfdu_tree::Class::CHECKED | dfdu_tree::Class::VCS_TRACKED;
    assert!(
        want.iter()
            .any(|c| c.0.ends_with("git/node_modules") && c.4 == tracked),
        "{want:?}"
    );

    let (e, agent, class) = remote_with(
        Some(RemoteRules {
            files,
            root: root.clone(),
        }),
        None,
    );
    let class = class.expect("both sides build the same rules");
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions {
            class: Some(class),
            ..JobOptions::default()
        },
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    assert_eq!(e.read(|m| m.status.fatal.clone()), None);
    assert_eq!(classes(&e), want);

    // A rescan below the root classifies with what the tree decided above it.
    let rescan = |e: &Engine<PortableBackend>| {
        let node = e.read(|m| {
            let r = m.tree.roots().next().unwrap();
            let d = m.tree.children(r).find(|&c| m.tree.name(c) == b"old");
            m.tree.node_ref(d.unwrap())
        });
        e.send(Command::Rescan { node });
        assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    };
    w("old/package-lock.json", b"{}");
    rescan(&local);
    rescan(&e);
    let want = classes(&local);
    assert_eq!(classes(&e), want);
    local.shutdown(Duration::from_secs(1));
    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
}

/// A link that ends during a scan keeps what arrived: the scan stops where it was, is
/// incomplete, and nothing more is scanned.
#[test]
fn a_link_lost_mid_scan_keeps_what_arrived() {
    use dfdu_engine::wire::{FromAgent, read_frame, write_frame};
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap().join("t");
    for i in 0..400 {
        let d = root.join(format!("d{}/e{}", i % 20, i % 13));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("f{i}")), vec![1u8; 100]).unwrap();
    }
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (agent_r, agent_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    // One worker, so the scan is still running when the link is cut.
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(PortableBackend::default(), 1, None, down_r, agent_w)
    });
    // Passes the agent's frames on until it has passed five chunks, then hangs up.
    let relay = std::thread::spawn(move || {
        let (mut r, mut w) = (agent_r, up_w);
        let mut buf = Vec::new();
        let mut chunks = 0;
        while chunks < 5 && read_frame(&mut r, &mut buf).unwrap() {
            if let Ok(FromAgent::Batch(msgs)) = FromAgent::decode(&buf, None) {
                chunks += msgs
                    .iter()
                    .filter(|m| matches!(m, dfdu_scan::ScanMsg::Chunk(_)))
                    .count();
            }
            write_frame(&mut w, &buf).unwrap();
        }
        // The agent's later writes fail once this end is gone.
        drop(r);
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
    e.send(Command::Scan {
        path: root.clone(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(30))).is_idle());
    relay.join().unwrap();
    let (why, incomplete, items) = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (
            m.status.disconnected.clone(),
            m.status.incomplete,
            m.tree.total(r).items,
        )
    });
    let why = why.expect("disconnected");
    assert!(why.contains("closed the link"), "{why}");
    let notices = e.read(|m| m.status.notices.clone());
    assert!(
        !notices.iter().any(|n| n.contains("invariant")),
        "{notices:?}"
    );
    assert!(incomplete);
    assert!(items > 0 && items < 400 + 20 + 20 * 13, "{items}");
    // Nothing more is scanned.
    let node = e.read(|m| m.tree.node_ref(m.tree.roots().next().unwrap()));
    e.send(Command::Rescan { node });
    assert!(e.wait_idle(Some(Duration::from_secs(5))).is_idle());
    let notice = e.read(|m| m.status.notices.last().cloned()).unwrap();
    assert!(notice.contains("nothing more is scanned"), "{notice}");
    e.shutdown(Duration::from_secs(1));
    // The agent ends once its output is gone.
    let _ = agent.join().unwrap();
}

fn node(e: &Engine<PortableBackend>, rel: &str) -> dfdu_tree::NodeRef {
    e.read(|m| {
        let mut ix = m.tree.roots().next().unwrap();
        for part in rel.split('/') {
            ix = m
                .tree
                .children(ix)
                .find(|&c| m.tree.name(c) == part.as_bytes())
                .unwrap_or_else(|| panic!("{rel}"));
        }
        m.tree.node_ref(ix)
    })
}

/// Deletes through the agent: verified and journalled on its side, the tree updated here, and
/// a staged item put back by an undo.
#[test]
fn deletes_run_on_the_agent() {
    use dfdu_engine::Mode;
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let root = base.join("t");
    let w = |p: &str, n: usize| {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; n]).unwrap();
    };
    w("gone/a", 4000);
    w("gone/sub/b", 4000);
    w("staged/c", 3000);
    w("changed", 100);
    w("kept", 100);
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
    let s = delete(vec![node(&e, "gone"), node(&e, "changed")], Mode::Permanent);
    assert_eq!((s.deleted, s.stale), (1, 1), "{:?}", s.problems);
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

    let s = delete(vec![node(&e, "staged")], Mode::Stage);
    assert_eq!(s.deleted, 1, "{:?}", s.problems);
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
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let root = tmp.path().join("t");
    let big: Vec<u8> = (0..4_000_000u32).map(|i| (i * 7 + i / 991) as u8).collect();
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

#[cfg(unix)]
fn wait_for(e: &Engine<PortableBackend>, cond: impl Fn(&dfdu_engine::Model) -> bool) {
    let t = std::time::Instant::now();
    while !e.read(&cond) {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
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
