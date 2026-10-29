use dfdu_scan::{DirId, JobId, ListingStatus, ScanMsg, SkipReason};
use dfdu_tree::{DirState, ErrKind, NodeFlags};

use super::super::JobPhase;
use super::super::test_util::{Listing, Rig, chunk, close, close_with};

#[test]
fn a_scan_completes_when_every_listing_has_closed() {
    let mut r = Rig::scan();
    let root = Listing {
        files: &[("f", 100)],
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    let a = Listing {
        files: &[("g", 50)],
        ..Listing::default()
    };
    r.apply(chunk(1, 0, 2, &a));
    r.apply(close(0, 1));
    assert_eq!(r.state(""), DirState::Pending, "a is still being listed");
    r.apply(close(1, 1));
    assert_eq!(
        (r.state(""), r.state("a")),
        (DirState::Complete, DirState::Complete)
    );
    assert_eq!(r.phase(), Some(JobPhase::Finishing));
    r.settle();
    assert_eq!(r.phase(), Some(JobPhase::Done));
    let t = r.m.tree.total(r.at(""));
    assert_eq!(t.alloc, 150);
    let s = &r.m.status.stats;
    assert_eq!((s.chunks, s.closes, s.entries), (2, 2, 3));
}

#[test]
fn a_close_before_its_chunks_waits_for_them() {
    let mut r = Rig::scan();
    r.apply(close(0, 2));
    assert!(!r.m.tree.dir(r.at("")).unwrap().listing_closed);
    let one = Listing {
        files: &[("f", 1)],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &one));
    assert_eq!(r.state(""), DirState::Pending);
    let two = Listing {
        files: &[("g", 1)],
        ..Listing::default()
    };
    r.apply(chunk(0, 1, 1, &two));
    assert_eq!(r.state(""), DirState::Complete);
    assert_eq!(r.m.status.stats.duplicate, 0);
}

#[test]
fn repeated_messages_are_applied_once() {
    let mut r = Rig::scan();
    let root = Listing {
        subdirs: &["a"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &root));
    let a = Listing {
        files: &[("f", 10)],
        ..Listing::default()
    };
    r.apply(chunk(1, 0, 2, &a));
    r.apply(chunk(1, 0, 2, &a));
    r.apply(close(1, 1));
    r.apply(close(1, 1));
    assert_eq!(r.m.status.stats.duplicate, 2);
    assert_eq!(r.m.tree.total(r.at("")).alloc, 10);
}

#[test]
fn a_message_for_an_unknown_directory_is_corrupt() {
    let mut r = Rig::scan();
    r.apply(close(9, 0));
    assert_eq!(r.m.status.stats.corrupt, 1);
    assert!(r.m.status.incomplete);
    assert_eq!(r.m.status.anomalies.len(), 1);
    // A close counting more chunks than arrived is corrupt too, and closes the listing.
    r.apply(chunk(0, 0, 1, &Listing::default()));
    r.apply(chunk(0, 1, 1, &Listing::default()));
    r.apply(close(0, 1));
    assert_eq!(r.m.status.stats.corrupt, 2);
    assert_eq!(r.state(""), DirState::Complete);
}

#[test]
fn skipped_subdirectories_are_closed_at_once() {
    let mut r = Rig::scan();
    let l = Listing {
        subdirs: &["nfs"],
        skip: Some(SkipReason::Network),
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &l));
    r.apply(close(0, 1));
    assert_eq!(r.state(""), DirState::Complete);
    assert_eq!(r.state("nfs"), DirState::Skipped);
    assert!(r.m.tree.flags(r.at("nfs")).has(NodeFlags::NETWORK));
}

#[test]
fn listings_that_end_early_keep_their_error() {
    let mut r = Rig::scan();
    let l = Listing {
        subdirs: &["a", "b"],
        ..Listing::default()
    };
    r.apply(chunk(0, 0, 1, &l));
    let partial = ListingStatus::Partial {
        panicked: 0,
        cancelled: 0,
        read_error: Some(ErrKind::PermissionDenied),
    };
    r.apply(close_with(1, 0, partial));
    r.apply(close_with(2, 0, ListingStatus::Failed(ErrKind::NotFound)));
    r.apply(close(0, 1));
    assert_eq!(r.state("a"), DirState::Partial);
    assert_eq!(r.m.tree.error(r.at("a")), Some(ErrKind::PermissionDenied));
    assert_eq!(r.state("b"), DirState::Failed);
    assert_eq!(r.m.tree.total(r.at("")).errors, 2);
}

#[test]
fn a_stalled_listing_shows_until_it_closes() {
    let mut r = Rig::scan();
    r.apply(chunk(0, 0, 1, &Listing::default()));
    r.apply(ScanMsg::Stalled {
        job: JobId(1),
        dir: DirId::ROOT,
    });
    assert_eq!(r.m.status.stalled.len(), 1);
    assert!(r.m.tree.dir(r.at("")).unwrap().stalled);
    r.apply(close(0, 1));
    assert!(r.m.status.stalled.is_empty());
    // A stall reported for a job that ended is not shown.
    r.settle();
    r.apply(ScanMsg::Stalled {
        job: JobId(1),
        dir: DirId::ROOT,
    });
    assert!(r.m.status.stalled.is_empty());
    assert_eq!(r.m.status.stats.discarded_job, 1);
}
