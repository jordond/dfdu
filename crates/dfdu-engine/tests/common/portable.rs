//! An engine on the portable backend, and the totals of its scans.

use std::path::Path;
use std::time::Duration;

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::Totals;

/// An engine of two workers, checking its invariants, with no journal kept.
pub fn engine() -> Engine<PortableBackend> {
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

/// The apparent size, disk use and entries of `t`.
pub fn sizes(t: Totals) -> (u64, u64, u64) {
    (t.apparent, t.alloc, t.items)
}

/// The totals of a fresh scan of `root`.
pub fn scanned(root: &Path) -> (u64, u64, u64) {
    let e = engine();
    e.send(Command::Scan {
        path: root.to_path_buf().into(),
        opts: JobOptions::default(),
    });
    assert!(e.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let t = e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())));
    e.shutdown(Duration::from_secs(1));
    t
}

/// The root's totals.
pub fn totals(e: &Engine<PortableBackend>) -> (u64, u64, u64) {
    e.read(|m| sizes(m.tree.total(m.tree.roots().next().unwrap())))
}
