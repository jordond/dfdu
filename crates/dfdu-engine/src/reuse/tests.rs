#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use dfdu_scan::JobOptions;
use dfdu_tree::{DirState, HostPath, Kind, Meta, Tree};

use super::*;
use crate::applier::{JobInfo, JobKind, JobPhase, Model};
use crate::test_support::{meta, node_at};
use crate::{Count, Engine};

/// Adds `name` under `parent` of `t` with `m`, counted above, a directory read to the end.
fn add(t: &mut Tree, parent: Ix, name: &str, m: &Meta) -> Ix {
    let ix = if m.kind == Kind::Dir {
        t.add_dir(parent, name.as_bytes(), m)
    } else {
        t.add_file(parent, name.as_bytes(), m, true, None)
    };
    let own = t.own(ix);
    t.propagate_add(parent, &own);
    if let Some(d) = t.dir_mut(ix) {
        d.state = DirState::Complete;
    }
    ix
}

/// A finished scan of `/p/a/c`: `c/d/f` (100 bytes) and two hard links to one inode, `c/l1`
/// and `c/d/l2` (50 bytes).
fn finished() -> Model {
    let mut t = Tree::new();
    let r = t.new_root(b"/p/a/c", &meta(Kind::Dir, 0, 10), true);
    t.dir_mut(r).unwrap().state = DirState::Complete;
    let d = add(&mut t, r, "d", &meta(Kind::Dir, 0, 11));
    add(&mut t, d, "f", &meta(Kind::File, 100, 12));
    let link = Meta {
        nlink: 2,
        ..meta(Kind::File, 50, 13)
    };
    add(&mut t, r, "l1", &link);
    let l2 = add(&mut t, d, "l2", &link);
    t.set_charged(l2, false);
    let mut m = Model {
        tree: t,
        ..Model::default()
    };
    m.status.jobs.push(JobInfo {
        id: dfdu_scan::JobId(1),
        kind: JobKind::Initial,
        target: m.tree.node_ref(r),
        phase: JobPhase::Done,
    });
    m
}

fn path(p: &str) -> HostPath {
    HostPath::from(p)
}

#[test]
fn a_finished_scan_is_copied_for_a_folder_above_it() {
    let m = finished();
    let s = scanned(&m, &path("/p"), 7).expect("copied");
    assert_eq!(s.root(), path("/p/a/c"));
    assert_eq!((s.since, s.full_scan), (7, 7));
    assert!(s.tree.check(&[]).is_ok());
    let r = s.tree.roots().next().unwrap();
    assert_eq!(
        s.tree.total(r),
        m.tree.total(m.tree.roots().next().unwrap())
    );
    // As loaded: no job has it, its listings are closed.
    let d = s.tree.dir(node_at(&s.tree, "d")).unwrap();
    assert_eq!(d.job, dfdu_scan::JobId(0));
    assert!(d.listing_closed && d.reported && d.pending == 0);
    assert_eq!(d.state, DirState::Complete);
}

#[test]
fn a_scan_not_settled_or_not_below_is_not_copied() {
    let m = finished();
    assert!(scanned(&m, &path("/p/a/c"), 0).is_none(), "the root itself");
    assert!(scanned(&m, &path("/q"), 0).is_none(), "not above");
    assert!(
        scanned(&m, &path("/p/a/cd"), 0).is_none(),
        "a name that starts alike"
    );
    let refuse: [fn(&mut Model); 5] = [
        |m| m.status.incomplete = true,
        |m| m.status.staged = Count { items: 1, bytes: 1 },
        |m| m.status.fatal = Some("x".into()),
        |m| m.status.jobs.clear(),
        |m| {
            let r = m.tree.roots().next().unwrap();
            m.tree.dir_mut(r).unwrap().state = DirState::Partial;
        },
    ];
    for (i, f) in refuse.iter().enumerate() {
        let mut m = finished();
        f(&mut m);
        assert!(scanned(&m, &path("/p"), 0).is_none(), "case {i}");
    }
}

/// [`finished`] while its scan still runs: `c` and `c/d` not read to the end, `c/d/e` not
/// read at all, `c/k` read to the end.
fn running() -> Model {
    let mut m = finished();
    m.status.jobs[0].phase = JobPhase::Running;
    let r = m.tree.roots().next().unwrap();
    let k = add(&mut m.tree, r, "k", &meta(Kind::Dir, 0, 14));
    add(&mut m.tree, k, "g", &meta(Kind::File, 10, 15));
    let d = node_at(&m.tree, "d");
    let e = add(&mut m.tree, d, "e", &meta(Kind::Dir, 0, 16));
    for ix in [r, d, e] {
        m.tree.dir_mut(ix).unwrap().state = DirState::Pending;
    }
    m
}

#[test]
fn a_running_scan_is_copied_with_what_it_has_not_finished() {
    let m = running();
    assert_eq!(keepable(&m, &path("/p")), m.tree.roots().next());
    let s = scanned(&m, &path("/p"), 0).expect("copied");
    let mut unfinished = s.unfinished.clone();
    unfinished.sort();
    assert_eq!(
        unfinished,
        [PathBuf::new(), PathBuf::from("d"), PathBuf::from("d/e")]
    );
    // Loaded as a snapshot of it is: shown incomplete until listed again.
    assert_eq!(
        s.tree.dir(node_at(&s.tree, "d")).unwrap().state,
        DirState::Partial
    );
    assert_eq!(
        s.tree.dir(node_at(&s.tree, "k")).unwrap().state,
        DirState::Complete
    );
    // The graft lists them again, below the folders between.
    let stats = [meta(Kind::Dir, 0, 1), meta(Kind::Dir, 0, 2)];
    let (_, mut changed) = graft(s, &path("/p"), &stats).expect("grafted");
    changed.sort();
    assert_eq!(changed, ["a", "a/c", "a/c/d", "a/c/d/e"].map(PathBuf::from));
    // Not before the root's stat is in.
    let mut m = running();
    let r = m.tree.roots().next().unwrap();
    m.tree.update_meta(r, &meta(Kind::Dir, 0, 0));
    assert_eq!(keepable(&m, &path("/p")), None);
    // A finished scan has nothing unfinished.
    assert!(
        scanned(&finished(), &path("/p"), 0)
            .unwrap()
            .unfinished
            .is_empty()
    );
}

#[test]
fn the_graft_puts_the_old_root_under_the_folder_and_the_folders_between() {
    let m = finished();
    let s = scanned(&m, &path("/p"), 0).unwrap();
    let old = m.tree.total(m.tree.roots().next().unwrap());
    let stats = [meta(Kind::Dir, 0, 1), meta(Kind::Dir, 0, 2)];
    let (t, changed) = graft(s, &path("/p"), &stats).expect("grafted");
    assert!(t.check(&[]).is_ok());
    assert_eq!(changed, [PathBuf::from("a")]);
    let r = t.roots().next().unwrap();
    assert_eq!(t.host_path(r), path("/p"));
    let c = node_at(&t, "a/c");
    // Joined as the tree joins, with `\` on a Windows host.
    assert_eq!(t.host_path(c), path("/p").join(b"a").join(b"c"));
    assert_eq!(t.total(c), old);
    assert_eq!(t.dev_ino(c), (1, 10));
    // Listed again by the resume: shown incomplete until then.
    for d in [r, node_at(&t, "a")] {
        assert_eq!(t.dir(d).unwrap().state, DirState::Cancelled);
    }
    assert_eq!(t.dir(c).unwrap().state, DirState::Complete);
    // One of the two links is charged.
    let charged = ["l1", "d/l2"]
        .iter()
        .filter(|p| {
            t.flags(node_at(&t, &format!("a/c/{p}")))
                .has(NodeFlags::CHARGED)
        })
        .count();
    assert_eq!(charged, 1);
}

#[test]
fn a_graft_needs_a_directory_stat_for_each_folder_down_to_the_old_root() {
    let m = finished();
    let dir = meta(Kind::Dir, 0, 1);
    let file = meta(Kind::File, 0, 1);
    for (above, stats) in [
        ("/p", vec![dir]),
        ("/p", vec![dir, dir, dir]),
        ("/p", vec![dir, file]),
        ("/q", vec![dir]),
    ] {
        let s = scanned(&m, &path("/p"), 0).unwrap();
        assert!(
            graft(s, &path(above), &stats).is_none(),
            "{above} {stats:?}"
        );
    }
    let s = scanned(&m, &path("/p/a"), 0).unwrap();
    let (_, changed) = graft(s, &path("/p/a"), &[dir]).expect("one level up");
    assert!(changed.is_empty());
}

/// The stats down to the old root `/p/a/c`, read with device and inode `read`, from `/p` on
/// device 1, with `c` stat'ed as `c` and `a` on device `a_dev`.
fn down(read: (u64, u64), c: Meta, a_dev: u64) -> Option<Down> {
    let old = path("/p/a/c");
    stats_down(&old, read, &path("/p"), |p| {
        Some(match p.as_bytes() {
            b"/p" => meta(Kind::Dir, 0, 1),
            b"/p/a" => Meta {
                dev: a_dev,
                ..meta(Kind::Dir, 0, 2)
            },
            _ => c,
        })
    })
}

#[test]
fn the_old_root_may_be_a_mount_of_its_own_but_not_a_folder_between() {
    let on = |dev, ino| Meta {
        dev,
        ..meta(Kind::Dir, 0, ino)
    };
    // On the same device: no mount to enter.
    let d = down((1, 10), on(1, 10), 1).expect("same device");
    assert_eq!((d.stats.len(), d.mount), (2, None));
    // The old root as read, on a device of its own: entered.
    let d = down((2, 10), on(2, 10), 1).expect("a mount");
    assert_eq!(d.mount, Some((2, 10)));
    // Not the directory read: it became a mount, or was replaced.
    assert!(down((1, 10), on(2, 10), 1).is_none(), "became a mount");
    assert!(down((2, 10), on(2, 11), 1).is_none(), "another root");
    assert!(down((2, 10), on(1, 10), 1).is_none(), "no longer a mount");
    // A mount between.
    assert!(down((1, 10), on(1, 10), 3).is_none());
    assert!(down((2, 10), on(2, 10), 3).is_none());
    // Not a directory.
    let file = Meta {
        dev: 2,
        ..meta(Kind::File, 0, 10)
    };
    assert!(down((2, 10), file, 1).is_none());
}

fn wait<B: dfdu_scan::Backend>(e: &Engine<B>) {
    assert!(e.wait_idle(Some(Duration::from_secs(10))).is_idle());
}

/// An engine that scanned `p` to the end.
fn scan(p: &HostPath) -> Engine<dfdu_scan::PortableBackend> {
    let e = Engine::portable(2);
    e.send(crate::Command::Scan {
        path: p.clone(),
        opts: JobOptions::default(),
    });
    wait(&e);
    e
}

/// The root's items and apparent size, and whether the scan is incomplete.
fn totals<B: dfdu_scan::Backend>(e: &Engine<B>) -> (u64, u64, bool) {
    e.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (
            m.tree.total(r).items,
            m.tree.total(r).apparent,
            m.status.incomplete,
        )
    })
}

/// What a fresh scan of `p` ends with.
fn fresh(p: &HostPath) -> (u64, u64, bool) {
    let e = scan(p);
    let t = totals(&e);
    e.shutdown(Duration::from_millis(200));
    t
}

/// A scan of a folder above another scan's root keeps it and ends as a fresh scan of the
/// folder does; when the old root is gone by then, the folder is scanned afresh.
#[test]
fn a_scan_above_ends_as_a_fresh_scan_does() {
    let tmp = tempfile::tempdir().unwrap();
    let top = dunce::canonicalize(tmp.path()).unwrap();
    let c = top.join("a").join("c");
    std::fs::create_dir_all(c.join("d")).unwrap();
    std::fs::write(c.join("d").join("f"), b"0123456789").unwrap();
    std::fs::write(top.join("a").join("g"), b"01234").unwrap();
    std::fs::write(top.join("h"), b"012").unwrap();
    let top_path = HostPath::local(&top);

    let old = scan(&HostPath::local(&c));
    assert!(old.keeps(&top_path));
    assert!(!old.keeps(&HostPath::local(&c)), "the root itself");
    assert!(
        !old.keeps(&HostPath::local(&top.join("nowhere"))),
        "not above"
    );
    let s = old.scanned(&top_path).expect("copied");
    old.shutdown(Duration::from_millis(200));
    let new = Engine::portable(2);
    assert!(new.scan_above(&top_path, Some(s), JobOptions::default()));
    wait(&new);
    assert_eq!(totals(&new), fresh(&top_path));
    new.shutdown(Duration::from_millis(200));

    let old = scan(&HostPath::local(&c));
    let s = old.scanned(&top_path).expect("copied");
    old.shutdown(Duration::from_millis(200));
    std::fs::remove_dir_all(&c).unwrap();
    let new = Engine::portable(2);
    assert!(
        !new.scan_above(&top_path, Some(s), JobOptions::default()),
        "afresh"
    );
    wait(&new);
    assert_eq!(totals(&new), fresh(&top_path));
    new.shutdown(Duration::from_millis(200));
}
