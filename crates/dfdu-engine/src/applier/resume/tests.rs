use std::sync::Arc;

use dfdu_scan::{JobId, Limits, PortableBackend, Shared};
use dfdu_tree::{DirState, Kind};

use super::rescans::kept_by_target;
use super::*;
use crate::applier::ApplierSettings;
use crate::test_support::meta;

mod cancel;

/// /r/{a/{b/, f}, c/}, every directory read to the end, as loaded; returns the tree and r,
/// a, b, c.
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
pub(super) fn tree() -> (Tree, [Ix; 4]) {
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

/// An applier with `t` in its model and a resume of its root `r` applied: the paths `changed`
/// below it changed since `since`, and `root` the root's stat. No worker runs.
pub(super) fn resumed(
    t: Tree,
    r: Ix,
    changed: &[&str],
    since: u64,
    root: Option<Meta>,
) -> (Applier<PortableBackend>, Model) {
    let shared = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
    let mut ap = Applier::new(shared, ApplierSettings::default());
    let mut m = Model::default();
    let node = t.node_ref(r);
    m.tree = t;
    ap.apply_command(
        &mut m,
        Command::Resume {
            node,
            opts: JobOptions::default(),
            changed: Some(changed.iter().map(PathBuf::from).collect()),
            since,
            root: root.map(Box::new),
        },
    );
    (ap, m)
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

fn keys(k: &Kept) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = k.nodes.keys().copied().collect();
    v.sort_unstable();
    v
}

/// A change in `b` lists `r`, `a` and `b` again, keeping `c`: the rescan is of the root
/// when it changed, of `b` alone when it did not.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn what_is_listed_again_keeps_the_rest() {
    let (t, [r, a, b, c]) = tree();
    let kept = kept_by_target(&t, r, &[r, b], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(c)]);
    let kept = kept_by_target(&t, r, &[a], &[a], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(b)]);
    let kept = kept_by_target(&t, r, &[b], &[b], &[]);
    assert_eq!(keys(&kept[0]), []);
    // A mount is scanned in full.
    assert_eq!(keys(&kept_by_target(&t, r, &[a], &[a], &[a])[0]), []);
}

/// A directory whose listing was not read to the end is listed again.
#[test]
fn an_incomplete_directory_is_not_kept() {
    let (mut t, [r, a, _, c]) = tree();
    t.dir_mut(c).unwrap().state = DirState::Partial;
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(a)]);
}

/// What the check finds is listed again: a directory whose mtime moved, and the parent of one
/// gone; a batch that could not be stat'ed to the end lists all of its directories and their
/// parents. The rescans start once the last batch is answered.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn what_the_check_finds_is_listed_again() {
    use dfdu_scan::ScanMsg;

    let (t, [r, a, b, c]) = tree();
    let (mut ap, mut m) = resumed(t, r, &[], 0, None);
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    let job = ap.jobs.keys().next().copied().unwrap();
    let mut v = Verify::new(&m.tree, r, &[], 0);
    for ix in [a, b, c] {
        v.add(&m.tree, ix);
    }
    v.add(&m.tree, r);
    assert_eq!(v.next.len(), 3, "the root is not checked");
    v.batches
        .push(Some(vec![m.tree.node_ref(a), m.tree.node_ref(b)]));
    v.waiting = 1;
    let refs = |ixs: &[Ix]| ixs.iter().map(|&i| m.tree.node_ref(i)).collect::<Vec<_>>();
    assert_eq!(v.answer(&m.tree, 0, &[0], &[1], true), refs(&[a, a]));
    assert!(!v.waiting());
    v.batches.push(Some(refs(&[b, c])));
    v.waiting = 1;
    assert_eq!(v.answer(&m.tree, 1, &[], &[], false), refs(&[b, c, a, r]));
    assert!(v.answer(&m.tree, 1, &[0], &[], true).is_empty(), "answered");

    // Where the walk's batches order the directories: by the walk, from the root down.
    let order = ap
        .resume
        .waiting
        .as_ref()
        .and_then(|w| w.verify.as_ref())
        .and_then(|v| v.batches[0].clone())
        .unwrap();
    let at = |ix: Ix| {
        order
            .iter()
            .position(|&o| o == m.tree.node_ref(ix))
            .unwrap()
    };
    let (gone, moved) = (u32::try_from(at(b)).unwrap(), u32::try_from(at(c)).unwrap());
    ap.apply(
        &mut m,
        ScanMsg::Verified {
            job: JobId(job),
            batch: 0,
            changed: vec![moved],
            gone: vec![gone],
            complete: true,
        },
        1,
    );
    assert!(ap.resume.waiting.is_none() && ap.has_work());
    let Some(Work::Resume(walk)) = ap.work.front() else {
        panic!("the walk goes on");
    };
    assert!(
        [a, c]
            .iter()
            .all(|&d| walk.wanted.contains(&m.tree.node_ref(d))),
        "listed again"
    );
    assert!(!walk.wanted.contains(&m.tree.node_ref(b)));
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    assert!(!ap.resuming());
}

/// The root is judged on the stat the resume is sent, as the directories it keeps are on
/// theirs: listed again when its mtime moved, is not older than the journal position, or is
/// not known, and scanned again in full when it is another directory.
#[test]
fn the_root_is_listed_again_unless_its_stat_vouches_for_it() {
    use dfdu_scan::ScanMsg;

    let loaded = Meta {
        mtime: 5,
        ..meta(Kind::Dir, 0, 1)
    };
    let cases = [
        (0, Some(5), false),
        (6, Some(5), false),
        (5, Some(5), true),
        (4, Some(5), true),
        (0, Some(6), true),
        (0, None, true),
    ];
    for (since, now, listed) in cases {
        let mut t = Tree::new();
        let r = t.new_root(b"/r", &loaded, true);
        let a = t.add_dir(r, b"a", &meta(Kind::Dir, 0, 2));
        let own = t.own(a);
        t.propagate_add(r, &own);
        for d in [r, a] {
            let rec = t.dir_mut(d).unwrap();
            rec.state = DirState::Complete;
            rec.outcome = rec.state;
            rec.listing_closed = true;
            rec.reported = true;
        }
        let root = t.node_ref(r);
        let now = now.map(|mtime| Meta { mtime, ..loaded });
        let (mut ap, mut m) = resumed(t, r, &[], since, now);
        while ap.has_work() {
            ap.work_step(&mut m);
        }
        let job = ap.jobs.keys().next().copied().unwrap();
        ap.apply(
            &mut m,
            ScanMsg::Verified {
                job: JobId(job),
                batch: 0,
                changed: vec![],
                gone: vec![],
                complete: true,
            },
            1,
        );
        while ap.has_work() {
            ap.work_step(&mut m);
        }
        let rescans = ap
            .jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.target == root)
            .count();
        assert_eq!(rescans, usize::from(listed), "{since} {now:?}");
    }
    // Another directory where the root was is scanned again in full, without a resume.
    let (t, [r, ..]) = tree();
    let root = t.node_ref(r);
    let (ap, _) = resumed(t, r, &[], 0, Some(meta(Kind::Dir, 0, 77)));
    assert!(!ap.resuming());
    assert_eq!(ap.jobs.values().filter(|j| j.target == root).count(), 1);
}

/// A kept directory goes to its scanner with the mtime it was loaded with.
#[test]
fn a_kept_directory_is_kept_with_its_loaded_mtime() {
    let (t, [r, a, _, c]) = tree();
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    let keep = kept[0].keep();
    let (dev, ino) = t.dev_ino(c);
    assert!(!keep.take(dev, ino, t.mtime(c) + 1), "its entries changed");
    let (dev, ino) = t.dev_ino(a);
    assert!(keep.take(dev, ino, t.mtime(a)));
}
