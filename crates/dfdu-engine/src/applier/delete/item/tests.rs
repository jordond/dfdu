use dfdu_clean::Mode;
use dfdu_tree::{Host, HostPath};

use super::*;
use crate::applier::Command;
use crate::applier::delete::tests::{rig, small};
use crate::applier::test_util::Rig;
use crate::test_util::meta;

/// What the tree refuses: a mount point, a protected node, one holding mounts (counted in the
/// message), one inside a read-only snapshot, and a snapshot without root.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn the_tree_refuses_mounts_protected_nodes_and_snapshots() {
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

/// Mounts below are counted in the message, which says "them" for more than one.
#[test]
fn mounts_below_are_counted() {
    let (mut t, [r, _, d]) = small();
    let e = t.add_dir(r, b"e", &meta(Kind::Dir, 0, 5));
    for m in [d, e] {
        t.set_flag(m, NodeFlags::MOUNT_ROOT, true);
    }
    assert_eq!(
        refusal(&t, r, false).as_deref(),
        Some("contains 2 mount points; unmount them first")
    );
}

/// A plan item carries the identity the deleter checks again and what is below it.
#[test]
fn a_plan_item_carries_its_identity_and_listings() {
    let (mut t, [_, f, d]) = small();
    t.dir_mut(d).unwrap().state = DirState::Complete;
    let item = plan_item(&t, d);
    assert_eq!(item.node, t.node_ref(d));
    assert_eq!(item.path, t.host_path(d));
    assert_eq!((item.identity.dev, item.identity.ino), (1, 3));
    assert_eq!(item.identity.kind, Kind::Dir);
    assert_eq!(item.bytes, 20);
    let listing = &item.inside[&HostPath::empty(t.host())];
    assert_eq!((listing.ino, listing.entries.len()), (3, 1));
    let file = plan_item(&t, f);
    assert_eq!(
        (file.identity.kind, file.identity.apparent),
        (Kind::File, 10)
    );
    assert!(file.inside.is_empty(), "a file has nothing inside");
}

/// Only the directories read to the end are listed: what a partial listing saw is not
/// compared.
#[test]
fn only_complete_directories_are_listed() {
    let (mut t, [r, _, d]) = small();
    assert!(listings(&t, r).is_empty(), "nothing read to the end");
    t.dir_mut(r).unwrap().state = DirState::Complete;
    let l = listings(&t, r);
    assert_eq!(l.len(), 1, "d is not complete");
    assert_eq!(l[&HostPath::empty(t.host())].entries.len(), 2);
    t.dir_mut(d).unwrap().state = DirState::Complete;
    assert_eq!(listings(&t, r).len(), 2);
}

/// The keys of what the scan saw inside an item are joined with the separator of the
/// tree's host, which is what the deleter there joins them with: a Windows client deleting
/// on a Unix host must not skip the check below the item's first level.
#[test]
fn listings_are_keyed_with_the_hosts_separator() {
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

/// What a tree loaded from a file holds, which no scan job of this session owns, is refused
/// until a resume takes it over; the tree's own refusals come after.
#[test]
fn an_item_not_refreshed_is_refused() {
    let r = rig();
    let (mut t, [_, f, d]) = small();
    let why = r.a.item_refusal(&t, f);
    assert!(
        why.is_some_and(|w| w.starts_with("is not refreshed yet: loaded from a file")),
        "a file of a loaded directory"
    );
    t.dir_mut(d).unwrap().job = dfdu_scan::JobId(1);
    t.set_flag(d, NodeFlags::MOUNT_ROOT, true);
    assert_eq!(
        r.a.item_refusal(&t, d).as_deref(),
        Some("is a mount point; unmount it first")
    );
}
