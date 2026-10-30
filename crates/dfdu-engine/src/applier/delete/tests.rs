use dfdu_clean::DeleterMsg;
use dfdu_tree::{ErrKind, Kind};

use super::*;
use crate::applier::Command;
use crate::applier::test_util::{Listing, Rig, chunk, close};

#[test]
fn a_summary_counts_each_result_and_keeps_the_problems() {
    let mut s = DeleteSummary::default();
    let n = NodeRef::from_bits;
    s.add(n(1), 100, &DeleteResult::Deleted);
    s.add(n(2), 50, &DeleteResult::Stale("changed".into()));
    s.add(n(3), 10, &DeleteResult::Aborted);
    let busy = DeleteResult::Failed {
        partial: true,
        err: ErrKind::Other,
        msg: "busy".into(),
    };
    s.add(n(4), 10, &busy);
    assert_eq!((s.deleted, s.bytes), (1, 100));
    assert_eq!((s.stale, s.refused, s.failed, s.aborted), (1, 0, 1, 1));
    let kept: Vec<NodeRef> = s.problems.iter().map(|p| p.0).collect();
    assert_eq!(kept, [n(2), n(3), n(4)]);
}

#[test]
fn problems_past_the_limit_are_counted_not_kept() {
    let mut s = DeleteSummary::default();
    for i in 0..PROBLEMS_KEPT as u64 + 5 {
        s.add(NodeRef::from_bits(i), 1, &DeleteResult::Aborted);
    }
    assert_eq!(s.aborted, PROBLEMS_KEPT + 5);
    assert_eq!(s.problems.len(), PROBLEMS_KEPT);
}

/// /r/{f, d/{g}}, as a scan leaves them; returns the tree and r, f, d.
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
pub(super) fn small() -> (Tree, [Ix; 3]) {
    use crate::test_util::meta;
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let f = t.add_file(r, b"f", &meta(Kind::File, 10, 2), false, None);
    let d = t.add_dir(r, b"d", &meta(Kind::Dir, 0, 3));
    let g = t.add_file(d, b"g", &meta(Kind::File, 20, 4), false, None);
    for (p, c) in [(r, f), (r, d), (d, g)] {
        let own = t.own(c);
        t.propagate_add(p, &own);
    }
    (t, [r, f, d])
}

/// A kept item is unmarked, and rescanned (marked cut) when it was cut for the delete or
/// partly deleted; a directory on another filesystem is not.
#[test]
fn a_kept_item_is_rescanned_when_its_totals_are_no_longer_known() {
    let (mut t, [_, f, d]) = small();
    let partial = DeleteResult::Failed {
        partial: true,
        err: ErrKind::Other,
        msg: "busy".into(),
    };
    t.set_deleting(f, true);
    assert!(!unmark_kept(&mut t, f, &partial), "a file has no totals");
    assert!(!t.flags(f).has(NodeFlags::DELETING));
    t.dir_mut(d).unwrap().state = DirState::Complete;
    assert!(!unmark_kept(&mut t, d, &DeleteResult::Aborted));
    assert!(unmark_kept(&mut t, d, &partial));
    assert!(t.dir(d).unwrap().cut);
    let rec = t.dir_mut(d).unwrap();
    (rec.cut, rec.state) = (true, DirState::Deleting);
    assert!(unmark_kept(&mut t, d, &DeleteResult::Aborted), "cut for it");
    let rec = t.dir_mut(d).unwrap();
    (rec.cut, rec.state) = (false, DirState::Skipped);
    assert!(!unmark_kept(&mut t, d, &partial), "on another filesystem");
}

/// Job 1 with the root listed: a file `f`, and a directory `a` still to read.
pub(super) fn rig() -> Rig {
    let mut r = Rig::scan();
    let l = Listing {
        files: &[("f", 10)],
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &l));
    r.apply(close(0, 1));
    r
}

/// A plan runs item by item: a deleted item leaves the tree, the summary counts it, and the
/// plan's end clears the progress and keeps the summary.
#[test]
fn a_plan_runs_to_its_summary() {
    let mut r = rig();
    let f = r.m.tree.node_ref(r.at("f"));
    r.command(Command::Delete {
        items: vec![f, f],
        mode: Mode::Permanent,
    });
    let plans = r.a.take_plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].items.len(),
        1,
        "an item named twice is deleted once"
    );
    let plan = plans[0].id;
    assert!(r.m.tree.flags(r.at("f")).has(NodeFlags::DELETING));
    assert_eq!(r.m.status.deleting.as_ref().map(|d| d.total), Some(1));
    r.a.apply_delete(
        &mut r.m,
        DeleterMsg::Item {
            plan,
            node: f,
            bytes: 10,
            result: DeleteResult::Deleted,
        },
    );
    r.settle();
    assert!(r.m.tree.resolve_visible(f).is_none());
    r.a.apply_delete(&mut r.m, DeleterMsg::Done { plan, btrfs: false });
    assert!(r.m.status.deleting.is_none());
    let s = r.m.status.last_delete.as_ref().unwrap();
    assert_eq!((s.plan, s.deleted, s.bytes), (plan, 1, 10));
}
