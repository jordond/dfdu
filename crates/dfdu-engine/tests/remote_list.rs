//! Listing on the agent's host for a prompt that goes to a folder: a directory's entries and
//! the mounts, asked without blocking and answered through the link; and an agent that is
//! greeted with another protocol version refuses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::time::Duration;

use dfdu_engine::wire::{self, FromAgent, ToAgent};
use dfdu_scan::PortableBackend;
use dfdu_tree::HostPath;

mod common;
use common::remote::remote;

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn a_remote_engine_lists_folders_and_mounts_on_the_agents_host() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    fs::create_dir(root.join("disk1")).unwrap();
    fs::write(root.join("notes"), b"x").unwrap();
    let (e, agent) = remote();
    let lister = e.lister().expect("a remote engine has a lister");
    assert_eq!(lister.host(), dfdu_tree::Host::LOCAL);

    let mut got = lister
        .list(&HostPath::local(&root))
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    got.sort_by(|a, b| a.name.cmp(&b.name));
    let got: Vec<_> = got.iter().map(|x| (x.name.as_slice(), x.dir)).collect();
    assert_eq!(got, [(&b"disk1"[..], true), (&b"notes"[..], false)]);

    let gone = lister
        .list(&HostPath::local(&root.join("gone")))
        .recv_timeout(WAIT)
        .unwrap();
    assert!(gone.is_err(), "{gone:?}");

    // An empty path is answered here, without asking the agent.
    let empty = lister
        .list(&HostPath::new(lister.host(), Vec::new()))
        .try_recv()
        .unwrap();
    assert_eq!(empty, Err("no folder to list".into()));

    // The mount list answers, with or without mounts the sandbox shows.
    let mounts = lister.mounts().recv_timeout(WAIT).unwrap();
    if let Ok(m) = &mounts {
        assert!(m.iter().all(|m| !m.point.is_empty()), "{m:?}");
    }

    // Once the link is gone an ask is not answered: its channel disconnects.
    e.shutdown(Duration::from_secs(1));
    agent.join().unwrap().unwrap();
    let rx = lister.list(&HostPath::local(&root));
    assert!(rx.recv_timeout(WAIT).is_err());
}

#[test]
fn a_local_engine_has_no_lister() {
    let e = dfdu_engine::Engine::portable(1);
    assert!(e.lister().is_none());
    e.shutdown(Duration::from_secs(1));
}

/// An agent greeted with a version it does not speak says so and stops; so an older agent
/// left on a host refuses a newer client, which then sends its own (`remote::boot`).
#[test]
fn an_agent_refuses_another_protocol_version() {
    for version in [wire::VERSION - 1, wire::VERSION + 1] {
        let mut down = Vec::new();
        let hello = ToAgent::Hello {
            version,
            root: b"/".to_vec(),
        };
        wire::write_frame(&mut down, &hello.encode()).unwrap();
        let (up_r, up_w) = std::io::pipe().unwrap();
        let r = dfdu_engine::agent::run(
            PortableBackend::default(),
            1,
            None,
            std::io::Cursor::new(down),
            up_w,
        );
        assert!(r.is_err(), "version {version} was accepted");
        let mut up_r = up_r;
        let mut buf = Vec::new();
        let mut said = Vec::new();
        while let Ok(true) = wire::read_frame(&mut up_r, &mut buf) {
            said.push(FromAgent::decode(&buf, None, dfdu_tree::Host::LOCAL).unwrap());
        }
        assert!(
            matches!(&said[..], [FromAgent::Hello { min, max, .. }, FromAgent::Fatal(why)]
                if *min == wire::VERSION && *max == wire::VERSION && why.contains("not spoken")),
            "{said:?}"
        );
    }
}

/// A path the agent cannot list is answered with why, and the link goes on: refusing the
/// message would end the agent, and the scan with it.
#[test]
fn an_agent_answers_a_path_it_cannot_list_and_goes_on() {
    let mut down = Vec::new();
    let mut frame = |m: ToAgent| wire::write_frame(&mut down, &m.encode()).unwrap();
    frame(ToAgent::Hello {
        version: wire::VERSION,
        root: b"/".to_vec(),
    });
    let bad = [
        Vec::new(),
        b"relative".to_vec(),
        vec![b'/'; wire::listing::MAX_PATH + 1],
    ];
    for (id, path) in bad.iter().enumerate() {
        frame(ToAgent::List {
            id: id as u64,
            path: path.clone(),
        });
    }
    let (mut up_r, up_w) = std::io::pipe().unwrap();
    let r = dfdu_engine::agent::run(
        PortableBackend::default(),
        1,
        None,
        std::io::Cursor::new(down),
        up_w,
    );
    assert!(r.is_ok(), "{r:?}");
    let mut buf = Vec::new();
    let mut answered = Vec::new();
    while let Ok(true) = wire::read_frame(&mut up_r, &mut buf) {
        if let FromAgent::Listed { id, result } =
            FromAgent::decode(&buf, None, dfdu_tree::Host::LOCAL).unwrap()
        {
            answered.push((id, result));
        }
    }
    assert_eq!(
        answered,
        [
            (0, Err("no folder to list".into())),
            (1, Err("the path is not absolute".into())),
            (2, Err("the path is too long".into())),
        ]
    );
}
