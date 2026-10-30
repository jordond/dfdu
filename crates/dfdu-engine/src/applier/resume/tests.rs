use super::rescans::kept_by_target;
use super::*;
use crate::test_util::meta;
use dfdu_scan::JobId;
use dfdu_tree::{DirState, Kind};

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

fn keys(k: &Kept) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = k.nodes.keys().copied().collect();
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
    assert!(keys(&kept_by_target(&t, r, &[a], &[a], &[a])[0]).is_empty());
}

/// A directory whose listing was not read to the end is listed again.
#[test]
fn an_incomplete_directory_is_not_kept() {
    let (mut t, [r, a, _, c]) = tree();
    t.dir_mut(c).unwrap().state = DirState::Partial;
    let kept = kept_by_target(&t, r, &[r], &[r], &[]);
    assert_eq!(keys(&kept[0]), [t.dev_ino(a)]);
}

/// Deletes of `items` sent to `ap`, refused as not refreshed by a cancelled resume.
fn refused_as_unverified<B: Backend>(
    ap: &mut Applier<B>,
    m: &mut Model,
    items: Vec<NodeRef>,
) -> usize {
    let before = m.status.notices.len();
    ap.apply_command(
        m,
        Command::Delete {
            items,
            mode: dfdu_clean::Mode::Permanent,
        },
    );
    m.status
        .notices
        .iter()
        .skip(before)
        .filter(|n| n.contains("its resume was cancelled"))
        .count()
}

/// While a resume walks the loaded tree, what would touch it is refused; a cancel while it
/// walks starts none of its rescans, which leaves the tree as loaded: the scan is incomplete,
/// and deletes refuse what the resume did not refresh, which a rescan may list again.
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
            since: 0,
            root: None,
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
    assert!(!ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
    assert!(m.status.incomplete);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![a, c]), 2);
    assert!(!unrefreshed(&m.tree, m.tree.resolve_visible(c).unwrap()));
    command(&mut ap, &mut m, Command::Rescan { node: c });
    assert!(ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
}

/// Once the walk has taken the tree over, the resume waits for the check of the directories
/// it keeps, refusing what would touch the tree as while it walks; a cancel then ends the
/// wait, starting none of the rescans, and an answer that comes late changes nothing. What
/// the check did not vouch for stays refused to deletes.
#[test]
fn a_resume_checking_refuses_and_cancels_what_follows() {
    use std::sync::Arc;

    use dfdu_clean::Mode;
    use dfdu_scan::{Limits, PortableBackend, ScanMsg, Shared};

    use crate::applier::Applier;

    let (t, [r, a, _, c]) = tree();
    let shared = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
    let mut ap = Applier::new(shared.clone());
    let mut m = Model::default();
    let (root, a, c) = (t.node_ref(r), t.node_ref(a), t.node_ref(c));
    m.tree = t;
    ap.apply_command(
        &mut m,
        Command::Resume {
            node: root,
            opts: JobOptions::default(),
            changed: Some(vec![]),
            since: 0,
            root: None,
        },
    );
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    // No worker runs here: the check's one batch waits.
    assert_eq!(shared.outstanding(), (1, 0));
    assert!(ap.resuming() && m.status.settling && m.status.busy());
    ap.apply_command(&mut m, Command::Rescan { node: c });
    ap.apply_command(
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
    ap.apply_command(&mut m, Command::CancelAll);
    while ap.has_work() {
        ap.work_step(&mut m);
        ap.check(&m).unwrap();
    }
    assert!(!ap.resuming());
    assert!(!ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
    assert!(m.status.incomplete);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![a, c]), 2);
    let job = ap.jobs.keys().next().copied().unwrap();
    ap.apply(
        &mut m,
        ScanMsg::Verified {
            job: JobId(job),
            batch: 0,
            changed: vec![0, 1],
            gone: vec![],
            complete: true,
        },
        1,
    );
    assert!(!ap.has_work() && !ap.resuming());
    ap.check(&m).unwrap();
}

/// What the check finds is listed again: a directory whose mtime moved, and the parent of one
/// gone; a batch that could not be stat'ed to the end lists all of its directories and their
/// parents. The rescans start once the last batch is answered.
#[test]
fn what_the_check_finds_is_listed_again() {
    use std::sync::Arc;

    use dfdu_scan::{Limits, PortableBackend, ScanMsg, Shared};

    use crate::applier::Applier;

    let (t, [r, a, b, c]) = tree();
    let shared = Arc::new(Shared::new(PortableBackend::default(), Limits::default()));
    let mut ap = Applier::new(shared);
    let mut m = Model::default();
    let root = t.node_ref(r);
    m.tree = t;
    ap.apply_command(
        &mut m,
        Command::Resume {
            node: root,
            opts: JobOptions::default(),
            changed: Some(vec![]),
            since: 0,
            root: None,
        },
    );
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
