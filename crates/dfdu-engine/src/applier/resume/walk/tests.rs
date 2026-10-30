use dfdu_tree::{Kind, Meta};

use super::*;
use crate::applier::resume::tests::{resumed, tree};
use crate::test_util::meta;

/// Taking a directory over gives it to the resume's job, registers its hard links, and keeps
/// the mounts and subvolumes below the root, which are scanned again in full.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_directory_taken_over_belongs_to_the_resume() {
    let (mut t, [r, a, b, c]) = tree();
    let linked = Meta {
        nlink: 2,
        ..meta(Kind::File, 10, 20)
    };
    let l = t.add_file(a, b"l", &linked, true, None);
    let sub = t.add_dir(
        r,
        b"sub",
        &Meta {
            dev: 2,
            ..meta(Kind::Dir, 0, 21)
        },
    );
    t.set_flag(b, NodeFlags::MOUNT_ROOT, true);
    t.set_flag(c, NodeFlags::MOUNT_ROOT, true);
    t.dir_mut(c).unwrap().state = DirState::Skipped;
    let mut links = LinkTable::default();
    let mut mounts = Vec::new();
    for ix in [r, a, b, c, sub] {
        own(&mut t, &mut links, 7, r, ix, &mut mounts);
    }
    for ix in [r, a, b, c, sub] {
        let d = t.dir(ix).unwrap();
        assert_eq!((d.job, d.dir_id), (JobId(7), DirId::ROOT));
    }
    assert!(links.has(t.dev_ino(l)));
    assert_eq!(links.charged(t.dev_ino(l), VISIBLE), Some(t.node_ref(l)));
    assert_eq!(
        mounts,
        [t.node_ref(b), t.node_ref(sub)],
        "not the root, nor a skipped one"
    );
    let mut mounts = Vec::new();
    t.set_flag(r, NodeFlags::MOUNT_ROOT, true);
    own(&mut t, &mut links, 7, r, r, &mut mounts);
    assert!(mounts.is_empty(), "the root is listed, not scanned in full");
}

/// A resume walks the loaded tree over and waits for the check of what it keeps, which no
/// worker answers here; a cancel ends the check, and the walk ends without its rescans, the
/// tree left as loaded.
#[test]
fn a_walk_waits_for_its_check_until_cancelled() {
    let (t, [r, ..]) = tree();
    let (mut ap, mut m) = resumed(t, r, &["a"], 0, None);
    let job = ap.jobs.keys().next().copied().unwrap();
    assert!(ap.resuming() && ap.resume_checks(job));
    assert!(!ap.resume_checks(job + 1));
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    let walk = ap.resume.waiting.as_ref().expect("waiting for the check");
    assert!(walk.walked && walk.verify.as_ref().is_some_and(Verify::waiting));
    assert!(
        m.tree.dir(r).is_some_and(|d| d.job == JobId(job)),
        "taken over"
    );
    assert!(ap.resume_checks(job));

    ap.cancel_resume(Some(job + 1));
    assert!(ap.resume.waiting.is_some(), "another job's cancel");
    ap.cancel_resume(Some(job));
    assert!(ap.resume.waiting.is_none() && ap.has_work());
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    assert!(!ap.resuming());
    assert!(
        m.status.incomplete,
        "what it did not list again is not refreshed"
    );
    assert_eq!(ap.jobs.len(), 1, "no rescan started");
}
