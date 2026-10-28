//! Remote scans over the wire protocol: an agent in a thread at the other end of two pipes
//! scans a directory, and the local engine builds the same tree a local scan builds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::time::Duration;

use dfdu_engine::{Command, Engine, RemoteIo, RemoteRules};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{JobOptions, PortableBackend};

mod common;
use common::remote::{opts, remote, remote_with};

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
                    c.degraded.map_or(0, |d| u8::try_from(d).unwrap() + 1),
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
