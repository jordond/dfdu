use std::path::PathBuf;

use dfdu_tree::ErrKind;

use super::super::test_util::{Listing, Rig, chunk, close};
use super::*;

/// A finished scan of `/r` on a device whose files can share extents: `f` and `empty` at the
/// root, `g` in `a`.
fn measured_scan() -> Rig {
    let mut r = Rig::scan();
    r.a.extent_devs.insert(1);
    let root = Listing {
        files: &[("f", 4096), ("empty", 0)],
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    let a = Listing {
        files: &[("g", 10)],
        ..Listing::default()
    };
    r.apply(chunk(1, 0, 2, &a));
    r.apply(close(1, 1));
    r.apply(close(0, 1));
    r.settle();
    r
}

/// The result `result` for the file at `path`, of measurement round `round`.
fn answer(r: &Rig, path: &str, round: u64, result: MeasureResult) -> MeasureMsg {
    let ix = r.at(path);
    MeasureMsg {
        round,
        node: r.m.tree.node_ref(ix),
        inode: r.m.tree.dev_ino(ix),
        result,
    }
}

#[test]
fn files_with_data_are_requested_by_directory() {
    let mut r = measured_scan();
    let reqs = r.a.take_measure_requests();
    let asked: Vec<(PathBuf, Vec<Vec<u8>>)> = reqs
        .iter()
        .map(|q| {
            (
                q.dir.clone(),
                q.files.iter().map(|f| f.name.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        asked,
        [
            ("/r".into(), vec![b"f".to_vec()]),
            ("/r/a".into(), vec![b"g".to_vec()])
        ]
    );
    assert_eq!(r.m.status.measuring.as_ref().map(|p| p.requested), Some(2));
    assert!(r.a.take_measure_requests().is_empty());
    // Every answer in, measuring is over.
    let failed = answer(&r, "f", 0, MeasureResult::Failed(ErrKind::Other));
    r.a.apply_measure(&mut r.m, failed, 1);
    assert_eq!(r.m.status.unmeasured, 1);
    assert_eq!(r.m.status.measuring.as_ref().map(|p| p.done), Some(1));
    let changed = answer(&r, "a/g", 0, MeasureResult::Changed);
    r.a.apply_measure(&mut r.m, changed, 2);
    assert!(r.m.status.measuring.is_none());
}

#[test]
fn a_round_without_answers_is_abandoned() {
    let mut r = measured_scan();
    r.a.tick(&mut r.m, MEASURE_TIMEOUT_NS - 1, None);
    assert_eq!(r.m.status.unmeasured, 0, "within the timeout");
    r.a.tick(&mut r.m, MEASURE_TIMEOUT_NS + 2, None);
    assert_eq!(r.m.status.unmeasured, 2);
    assert!(r.m.status.measuring.is_none());
    let notice = r.m.status.notices.last().map(String::as_str);
    assert!(notice.is_some_and(|n| n.starts_with("extent measurement stopped")));
    // The abandoned round's answers are ignored.
    let late = answer(&r, "f", 0, MeasureResult::Failed(ErrKind::Other));
    r.a.apply_measure(&mut r.m, late, MEASURE_TIMEOUT_NS + 3);
    assert_eq!(r.m.status.unmeasured, 2);
}

#[test]
fn nothing_is_measured_without_a_device_that_shares_extents() {
    let mut r = Rig::scan();
    let root = Listing {
        files: &[("f", 4096)],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    r.apply(close(0, 1));
    r.settle();
    assert!(r.a.take_measure_requests().is_empty());
    assert!(r.m.status.measuring.is_none());
}
