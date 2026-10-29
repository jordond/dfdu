use super::super::Tree;
use super::super::test_util::{add_dir, add_file, dir_meta};
use super::*;

/// Sequence numbers below 64 and above it are each new once, in any order.
#[test]
fn mark_seq_reports_each_chunk_once() {
    let mut d = DirRec::new();
    for seq in [0, 63, 64, 1000, 5, 65] {
        assert!(d.mark_seq(seq), "{seq} is new");
    }
    for seq in [0, 63, 64, 1000, 5, 65] {
        assert!(!d.mark_seq(seq), "{seq} is a duplicate");
    }
    assert!(d.mark_seq(999) && d.mark_seq(1001));
}

#[test]
fn close_records_the_outcome_and_frees_the_filter() {
    let mut d = DirRec::new();
    assert!(!d.state.is_terminal());
    d.mark_seq(500);
    d.close(DirState::Partial);
    assert!(d.listing_closed);
    assert_eq!(d.outcome, DirState::Partial);
    assert_eq!(d.seen_high.len(), 0);
    assert!(DirState::Partial.is_terminal());
}

/// Children added across several reservations come back in order, and the iterator knows how
/// many are left.
#[test]
fn children_span_every_block_in_order() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &dir_meta(), true);
    let mut want = Vec::new();
    for (chunk, n) in [3, 5, 9].into_iter().enumerate() {
        t.reserve_children(root, n);
        for i in 0..n {
            want.push(add_file(&mut t, root, &format!("c{chunk}-{i}"), 1));
        }
        want.push(add_dir(&mut t, root, &format!("d{chunk}")));
    }
    let mut it = t.children(root);
    assert_eq!(it.len(), want.len());
    let first = it.next();
    assert_eq!(it.size_hint(), (want.len() - 1, Some(want.len() - 1)));
    let got: Vec<Ix> = first.into_iter().chain(it).collect();
    assert_eq!(got, want);
    let leaf = want[0];
    assert_eq!(t.children(leaf).len(), 0, "a file has no children");
}
