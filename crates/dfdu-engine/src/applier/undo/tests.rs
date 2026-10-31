use super::super::test_util::{Listing, Rig, chunk, close};
use super::super::{JobKind, JobPhase};
use super::*;
use crate::test_support::meta;

#[test]
fn a_plan_can_be_undone_until_its_last_item_is_gone() {
    let mut r = Rig::scan();
    r.a.add_undoable(3, Mode::Trash, 100);
    r.a.add_undoable(3, Mode::Trash, 20);
    r.a.refresh_undo(&mut r.m);
    let u = r.m.status.undo.as_ref().unwrap();
    assert_eq!((u.plan, u.mode, u.items, u.bytes), (3, Mode::Trash, 2, 120));
    // The newest plan is the one offered.
    r.a.add_undoable(5, Mode::Stage, 1);
    r.a.refresh_undo(&mut r.m);
    assert_eq!(r.m.status.undo.as_ref().map(|u| u.plan), Some(5));
    r.a.forget_undoable(5, 1);
    r.a.forget_undoable(3, 100);
    r.a.refresh_undo(&mut r.m);
    let u = r.m.status.undo.as_ref().unwrap();
    assert_eq!((u.plan, u.items, u.bytes), (3, 1, 20));
    r.a.forget_undoable(3, 20);
    r.a.refresh_undo(&mut r.m);
    assert!(r.m.status.undo.is_none());
}

#[test]
fn a_path_resolves_to_its_deepest_directory_in_the_tree() {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let a = t.add_dir(root, b"a", &meta(Kind::Dir, 0, 2));
    let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 3));
    t.add_file(a, b"f", &meta(Kind::File, 1, 4), false, None);
    let at = |p: &str| node_at(&t, &p.into());
    assert_eq!(at("/r/a/b"), Some(b));
    assert_eq!(at("/r/a/b/gone/deeper"), Some(b));
    assert_eq!(at("/r/a/f"), Some(a), "a file is not a directory to rescan");
    assert_eq!(at("/r"), Some(root));
    assert_eq!(at("/elsewhere/a"), None);
}

#[test]
fn what_an_undo_put_back_is_rescanned_from_the_topmost_directory() {
    let mut r = Rig::scan();
    let root = Listing {
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    let a = Listing {
        subdirs: &["b"],
        ..Listing::default()
    };
    r.apply(chunk(1, 0, 2, &a));
    for dir in [2, 1, 0] {
        r.apply(close(dir, 1));
    }
    r.settle();
    r.a.rescan_paths = vec!["/r/a/b/x".into(), "/r/a/y".into(), "/r/a/b/z".into()];
    r.a.rescan_paths(&mut r.m);
    let target = r.m.tree.node_ref(r.at("a"));
    let rescans: Vec<_> =
        r.a.jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.phase == JobPhase::Running)
            .map(|j| j.target)
            .collect();
    assert_eq!(rescans, [target]);
    assert_eq!(r.a.rescan_paths, Vec::<dfdu_tree::HostPath>::new());
}
