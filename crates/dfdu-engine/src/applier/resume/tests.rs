use super::*;
use crate::test_util::meta;
use dfdu_tree::Kind;

/// /r/{a/{b/, f}, c/}, every directory read to the end, as loaded; returns the tree and r,
/// a, b, c.
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn tree() -> (Tree, [Ix; 4]) {
    let mut t = Tree::new();
    let r = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
    let add = |t: &mut Tree, p: Ix, ix: Ix| {
        let own = t.own(ix);
        t.propagate_add(p, &own);
        ix
    };
    let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
    let a = add(&mut t, r, a);
    let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 3));
    let b = add(&mut t, a, b);
    let c = t.add_dir(r, b"c", &meta(Kind::Dir, 0, 4));
    let c = add(&mut t, r, c);
    let f = t.add_file(a, b"f", &meta(Kind::File, 0, 5), false, None);
    add(&mut t, a, f);
    for d in [r, a, b, c] {
        let rec = t.dir_mut(d).unwrap();
        rec.state = DirState::Complete;
        rec.outcome = rec.state;
        rec.listing_closed = true;
        rec.reported = true;
    }
    (t, [r, a, b, c])
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn changed_paths_name_their_directory_or_its_nearest_ancestor() {
    let (t, [r, a, b, c]) = tree();
    let paths = ["a/b", "c", "a/gone/deeper", "a/f", ""].map(PathBuf::from);
    assert_eq!(changed_dirs(&t, r, &paths), [b, c, a, a, r]);
}

#[test]
fn only_the_topmost_changed_directories_are_kept() {
    let (t, [_, a, b, c]) = tree();
    let mut want = vec![a, c];
    want.sort_unstable_by_key(|d| d.0);
    assert_eq!(topmost(&t, vec![b, c, a, b]), want);
}

fn keys(k: &Option<Box<Kept>>) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = k.as_ref().unwrap().nodes.keys().copied().collect();
    v.sort_unstable();
    v
}

/// A change in `b` lists `r`, `a` and `b` again, keeping `c`: the rescan is of the root
/// when it changed, of `b` alone when it did not.
#[test]
fn what_is_listed_again_keeps_the_rest() {
    let (t, [r, a, b, c]) = tree();
    let kept = kept_by_target(&t, r, &[r, b], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(c)]);
    let kept = kept_by_target(&t, r, &[a], &[a], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(b)]);
    let kept = kept_by_target(&t, r, &[b], &[b], &[]);
    assert!(keys(&kept[0]).is_empty());
    // A mount is scanned in full.
    assert!(kept_by_target(&t, r, &[c], &[c], &[c])[0].is_none());
}

/// A directory whose listing was not read to the end is listed again.
#[test]
fn an_incomplete_directory_is_not_kept() {
    let (mut t, [r, a, _, c]) = tree();
    t.dir_mut(c).unwrap().state = DirState::Partial;
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(a)]);
}

/// While a resume walks the loaded tree, what would touch it is refused; a cancel while it
/// walks cancels the rescans it starts once it has walked it, which leave the tree as loaded.
#[test]
fn a_resume_walking_refuses_and_cancels_what_follows() {
    use std::sync::Arc;

    use dfdu_clean::Mode;
    use dfdu_scan::{Limits, PortableBackend, Shared};

    use crate::applier::Applier;

    let (t, [r, a, _, c]) = tree();
    let shared = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
    let mut ap = Applier::new(shared);
    let mut m = Model::default();
    let (root, a, c) = (t.node_ref(r), t.node_ref(a), t.node_ref(c));
    m.tree = t;
    let command = |ap: &mut Applier<PortableBackend>, m: &mut Model, cmd| {
        ap.apply_command(m, cmd);
        ap.check(m).unwrap();
    };
    command(
        &mut ap,
        &mut m,
        Command::Resume {
            node: root,
            opts: JobOptions::default(),
            changed: Some(vec![PathBuf::from("a/b"), PathBuf::from("c")]),
        },
    );
    assert!(ap.resuming());
    command(&mut ap, &mut m, Command::Rescan { node: c });
    command(
        &mut ap,
        &mut m,
        Command::Delete {
            items: vec![a],
            mode: Mode::Permanent,
        },
    );
    let refused = m
        .status
        .notices
        .iter()
        .filter(|n| n.contains("still being taken over"))
        .count();
    assert_eq!(refused, 2, "{:?}", m.status.notices);
    command(&mut ap, &mut m, Command::CancelAll);
    while ap.has_work() {
        ap.work_step(&mut m);
        ap.check(&m).unwrap();
    }
    let rescans: Vec<JobPhase> = ap
        .jobs
        .values()
        .filter(|j| j.kind == JobKind::Rescan)
        .map(|j| j.phase)
        .collect();
    assert!(!rescans.is_empty());
    assert!(
        rescans.iter().all(|&p| p == JobPhase::Cancelled),
        "{rescans:?}"
    );
    assert!(!unrefreshed(&m.tree, m.tree.resolve_visible(c).unwrap()));
}

/// A kept directory goes to its scanner with the mtime it was loaded with.
#[test]
fn a_kept_directory_is_kept_with_its_loaded_mtime() {
    let (t, [r, a, _, c]) = tree();
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    let keep = kept[0].as_ref().unwrap().keep();
    let (dev, ino) = t.dev_ino(c);
    assert!(!keep.take(dev, ino, t.mtime(c) + 1), "its entries changed");
    let (dev, ino) = t.dev_ino(a);
    assert!(keep.take(dev, ino, t.mtime(a)));
}
