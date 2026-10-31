//! Extent measurement during a scan: a directory is measured once its listing closes, while
//! the rest of its job is still being scanned.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use dfdu_sim::{Fault, SimPiece, Spec, SpecBuilder};
use dfdu_tree::{DirState, Kind, NodeFlags};

use common::{cfg, find, for_seeds, scan_root, state};

/// The root holds `a`, with two 8 KiB clones `x` and `y` sharing all their data, and `s`, whose
/// read never returns, on a filesystem that shares extents.
fn clones_beside_stuck_dir() -> Spec {
    let mut b = SpecBuilder::new();
    let a = b.dir(dfdu_sim::ROOT, "a");
    let clones = [b.file(a, "x", 8192), b.file(a, "y", 8192)];
    let s = b.dir(dfdu_sim::ROOT, "s");
    b.files(s, "f", 2);
    b.fault(
        s,
        Fault::Stall {
            block: 0,
            dur_ns: None,
        },
    );
    let mut spec = b.build();
    spec.dev_fs.insert(1, (0, true));
    let mut phys = 1 << 20;
    for (i, n) in spec.nodes.iter_mut().enumerate() {
        if n.meta.kind != Kind::File {
            continue;
        }
        let shared = clones.contains(&u32::try_from(i).unwrap());
        n.pieces = vec![SimPiece {
            phys: if shared { 4096 } else { phys },
            len: n.meta.alloc,
            encoded: false,
            shared,
        }];
        phys += n.meta.alloc;
    }
    spec
}

#[test]
fn a_closed_directory_is_measured_while_its_job_runs() {
    for_seeds(clones_beside_stuck_dir, cfg(), |sim| {
        scan_root(sim);
        for _ in 0..200 {
            if state(sim, "a") != DirState::Pending {
                break;
            }
            for w in 0..sim.workers.len() {
                sim.step_worker(w);
                sim.flush_worker(w);
            }
            sim.apply_all().unwrap();
        }
        assert_eq!(state(sim, "a"), DirState::Complete);
        assert_eq!(state(sim, "s"), DirState::Pending);
        let a = find(sim, "a");
        // Until measured, the clones count their data twice.
        assert_eq!(sim.model.tree.total(a).alloc, 4096 + 2 * 8192);
        assert!(sim.model.status.measuring.is_some());

        sim.applier_work().unwrap();
        assert_eq!(sim.answer_measurements().unwrap(), 2);
        assert_eq!(sim.model.status.running_jobs(), 1, "s holds the job open");
        for f in ["a/x", "a/y"] {
            assert!(sim.model.tree.flags(find(sim, f)).has(NodeFlags::MEASURED));
        }
        assert_eq!(sim.model.tree.total(a).alloc, 4096 + 8192);
        // The scan may still find more to measure.
        assert!(sim.model.status.measuring.is_some());
    });
}
