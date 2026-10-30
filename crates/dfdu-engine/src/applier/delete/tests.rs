use dfdu_clean::DeleterMsg;
use dfdu_tree::{ErrKind, Kind};

use super::item::listings;
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

/// The keys of what the scan saw inside an item are joined with the separator of the
/// tree's host, which is what the deleter there joins them with: a Windows client deleting
/// on a Unix host must not skip the check below the item's first level.
#[test]
fn listings_are_keyed_with_the_hosts_separator() {
    use crate::test_util::meta;
    use dfdu_tree::{Host, HostPath};
    for host in [Host::Unix, Host::Windows] {
        let mut t = Tree::new();
        t.set_host(host);
        let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
        let item = t.add_dir(root, b"item", &meta(Kind::Dir, 0, 2));
        let a = t.add_dir(item, b"a", &meta(Kind::Dir, 0, 3));
        let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 4));
        t.add_file(b, b"f", &meta(Kind::File, 1, 5), false, None);
        for d in [item, a, b] {
            t.dir_mut(d).expect("directory").state = DirState::Complete;
        }
        let mut keys: Vec<HostPath> = listings(&t, item).into_keys().collect();
        keys.sort();
        let sep = char::from(host.sep());
        let want =
            ["", "a", &format!("a{sep}b")].map(|k| HostPath::new(host, k.as_bytes().to_vec()));
        assert_eq!(keys, want, "{host:?}");
    }
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
fn small() -> (Tree, [Ix; 3]) {
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

/// What the tree refuses: a mount point, a protected node, one holding mounts (counted in the
/// message), one inside a read-only snapshot, and a snapshot without root.
#[test]
fn the_tree_refuses_mounts_protected_nodes_and_snapshots() {
    use super::item::refusal;
    let (mut t, [r, f, d]) = small();
    assert_eq!(refusal(&t, d, false), None);
    t.set_flag(d, NodeFlags::MOUNT_ROOT, true);
    assert_eq!(
        refusal(&t, d, false).as_deref(),
        Some("is a mount point; unmount it first")
    );
    assert_eq!(
        refusal(&t, r, false).as_deref(),
        Some("contains 1 mount point; unmount it first")
    );
    t.set_flag(d, NodeFlags::MOUNT_ROOT, false);
    t.set_protection(f, true, false);
    assert!(refusal(&t, f, false).is_some_and(|w| w.starts_with("is protected")));
    t.set_protection(f, false, false);
    t.set_flag(d, NodeFlags::RO_SUBVOL, true);
    let g = t.children(d).next().unwrap();
    assert_eq!(
        refusal(&t, g, true).as_deref(),
        Some("is in the read-only snapshot d")
    );
    assert_eq!(
        refusal(&t, d, false).as_deref(),
        Some("is a read-only snapshot: deleting it needs root")
    );
    assert_eq!(
        refusal(&t, r, false).as_deref(),
        Some("contains a read-only snapshot: deleting it needs root")
    );
    assert_eq!(refusal(&t, d, true), None, "root deletes snapshots");
}

/// A plan item carries the identity the deleter checks again and what is below it.
#[test]
fn a_plan_item_carries_its_identity_and_listings() {
    let (mut t, [_, _, d]) = small();
    t.dir_mut(d).unwrap().state = DirState::Complete;
    let item = super::item::plan_item(&t, d);
    assert_eq!(item.node, t.node_ref(d));
    assert_eq!((item.identity.dev, item.identity.ino), (1, 3));
    assert_eq!(item.identity.kind, Kind::Dir);
    assert_eq!(item.bytes, 20);
    let listing = &item.inside[&dfdu_tree::HostPath::empty(t.host())];
    assert_eq!((listing.ino, listing.entries.len()), (3, 1));
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
fn rig() -> Rig {
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

/// An item whose directory, or a directory above or below it, a rescan is reading is refused
/// until the rescan swaps in.
#[test]
fn an_item_being_rescanned_is_refused() {
    let mut r = rig();
    let a = r.m.tree.node_ref(r.at("a"));
    r.command(Command::Rescan { node: a });
    let refused = |r: &Rig, path| r.a.item_refusal(&r.m.tree, r.at(path));
    assert!(refused(&r, "a").is_some_and(|w| w.contains("being rescanned")));
    assert!(refused(&r, "").is_some(), "above it");
    assert_eq!(refused(&r, "f"), None);
    r.command(Command::Delete {
        items: vec![a],
        mode: Mode::Permanent,
    });
    assert!(r.a.take_plans().is_empty());
    let last = r.m.status.notices.last().unwrap();
    assert!(last.starts_with("delete: a or something in it"), "{last}");
}

/// Deletes an earlier session left unfinished are told of once, counted, with the journal's
/// notes.
#[test]
fn unfinished_deletes_are_noticed() {
    let mut r = rig();
    let before = r.m.status.notices.len();
    r.a.apply_delete(
        &mut r.m,
        DeleterMsg::Unfinished {
            items: vec!["/x".into(), "/y".into(), "/z".into()],
            notes: vec!["torn record".into()],
        },
    );
    let new = &r.m.status.notices[before..];
    assert_eq!(new.len(), 2, "{new:?}");
    assert!(new[0].contains("left 3 deletes unfinished"), "{}", new[0]);
    assert!(new[0].ends_with(": /x; and 2 more"), "{}", new[0]);
    assert_eq!(new[1], "deletion journal: torn record");
}
