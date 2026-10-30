use dfdu_scan::{Limits, PortableBackend};

use super::*;
use crate::applier::resume::tests::tree;

/// The directories reached are checked unless they are the root, named by the journal, on
/// another device, a mount root, or were not read when scanned.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn what_is_checked_leaves_out_what_is_listed_or_scanned_anyway() {
    let (mut t, [r, a, b, c]) = tree();
    let mut v = Verify::new(&t, r, &[c], 0);
    for ix in [r, a, b, c] {
        v.add(&t, ix);
    }
    assert_eq!(v.next, [t.node_ref(a), t.node_ref(b)]);

    t.set_flag(a, NodeFlags::MOUNT_ROOT, true);
    t.dir_mut(b).unwrap().state = DirState::Skipped;
    let mut v = Verify::new(&t, r, &[], 0);
    for ix in [a, b, c] {
        v.add(&t, ix);
    }
    assert_eq!(v.next, [t.node_ref(c)]);
}

/// Only full batches go until the walk has reached everything; then the rest goes too,
/// without what is gone since it was reached.
#[test]
fn batches_go_full_or_at_the_end() {
    let (mut t, [r, a, b, c]) = tree();
    let scan = Shared::new(PortableBackend::default(), Limits::default());
    let mut v = Verify::new(&t, r, &[], 0);
    for ix in [a, b, c] {
        v.add(&t, ix);
    }
    v.send(&t, &scan, 1, false);
    assert!(!v.waiting(), "a batch is not full");
    assert_eq!(v.next.len(), 3);
    t.detach(c);
    v.send(&t, &scan, 1, true);
    assert!(v.waiting());
    assert!(v.next.is_empty());
    assert_eq!(v.batches, [Some(vec![t.node_ref(a), t.node_ref(b)])]);

    // Nothing left to stat sends no batch.
    let mut v = Verify::new(&t, r, &[], 0);
    v.next.push(t.node_ref(c));
    v.send(&t, &scan, 1, true);
    assert!(v.batches.is_empty() && !v.waiting());
}

/// An answer lists again the directories that changed and the parents of those gone; a batch
/// stat'ed only in part lists all of its directories and their parents. A batch is answered
/// once.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn an_answer_lists_again_what_changed() {
    let (t, [r, a, b, c]) = tree();
    let refs = |ixs: &[Ix]| ixs.iter().map(|&i| t.node_ref(i)).collect::<Vec<_>>();
    let mut v = Verify::new(&t, r, &[], 0);
    v.batches = vec![Some(refs(&[a, b])), Some(refs(&[c]))];
    v.waiting = 2;
    assert_eq!(v.answer(&t, 0, &[1], &[0, 7], true), refs(&[b, r]));
    assert!(v.waiting());
    assert!(v.answer(&t, 0, &[1], &[], true).is_empty(), "answered");
    assert!(v.answer(&t, 5, &[0], &[], true).is_empty(), "no such batch");
    assert_eq!(v.answer(&t, 1, &[], &[], false), refs(&[c, r]));
    assert!(!v.waiting());
}
