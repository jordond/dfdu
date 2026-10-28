//! An imported scan in the engine: browsed as loaded, and nothing touches the filesystem
//! (its paths may be another machine's).

use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions, Mode};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_store::ncdu::{self, Limits};

const EXPORT: &[u8] = br#"[1,2,{},[{"name":"/nowhere"},[{"name":"d","dsize":4096},{"name":"f","asize":5,"dsize":4096}]]]"#;

#[test]
fn imported_scans_are_read_only() {
    let im = ncdu::import(EXPORT, &Limits::default()).unwrap();
    let e = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            imported: true,
            workers: 1,
            ..Default::default()
        },
    );
    e.load(im.tree);
    let (d, total) = e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (
            m.tree.node_ref(m.tree.children(r).next().unwrap()),
            m.tree.total(r),
        )
    });
    assert_eq!(total.alloc, 8192);
    for cmd in [
        Command::Rescan { node: d },
        Command::Delete {
            items: vec![d],
            mode: Mode::Permanent,
        },
        Command::Scan {
            path: "/nowhere".into(),
            opts: JobOptions::default(),
        },
    ] {
        e.send(cmd);
    }
    assert!(e.wait_idle(Some(Duration::from_secs(5))));
    e.read(|m| {
        assert_eq!(m.tree.roots().count(), 1);
        assert_eq!(m.tree.total(m.tree.roots().next().unwrap()), total);
        assert_eq!(
            m.status
                .notices
                .iter()
                .filter(|n| n.contains("imported scan is read-only"))
                .count(),
            3,
            "{:?}",
            m.status.notices
        );
        assert!(m.status.last_delete.is_none());
    });
    e.shutdown(Duration::from_secs(1));
}
