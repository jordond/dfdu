use dfdu_scan::{DirId, JobId, ScanMsg};
use dfdu_tree::{DirState, ErrKind};

use super::super::test_util::{Listing, Rig, SEQ, chunk, close};
use super::super::{Command, JobPhase};

#[test]
fn quiescence_closes_listings_that_never_closed_as_lost() {
    let mut r = Rig::two_pending();
    r.apply(chunk(1, 0, 3, &Listing::default()));
    r.apply(close(1, 1));
    r.apply(ScanMsg::Quiescent { submitted_seq: SEQ });
    r.settle();
    assert_eq!(r.state("a"), DirState::Complete);
    assert_eq!(r.state("b"), DirState::Lost);
    assert_eq!(r.m.tree.error(r.at("b")), Some(ErrKind::Other));
    assert_eq!(r.m.status.stats.lost, 1);
    assert!(r.m.status.incomplete);
    assert_eq!(r.state(""), DirState::Complete);
    assert_eq!(r.phase(), Some(JobPhase::Done));
}

#[test]
fn quiescence_leaves_stalled_listings_and_later_submissions_alone() {
    let mut r = Rig::two_pending();
    r.apply(ScanMsg::Stalled {
        job: JobId(1),
        dir: DirId(1),
    });
    // Quiescence from before the job was submitted says nothing about it.
    r.apply(ScanMsg::Quiescent {
        submitted_seq: SEQ - 1,
    });
    r.settle();
    assert_eq!(r.state("b"), DirState::Pending);
    r.apply(ScanMsg::Quiescent { submitted_seq: SEQ });
    r.settle();
    assert_eq!(r.state("a"), DirState::Pending, "stalled, not lost");
    assert_eq!(r.state("b"), DirState::Lost);
    assert_eq!(r.m.status.stats.lost, 1);
}

#[test]
fn the_cancel_walk_cancels_only_pending_directories() {
    let mut r = Rig::two_pending();
    r.apply(chunk(1, 0, 3, &Listing::default()));
    r.apply(close(1, 1));
    r.command(Command::Cancel { job: JobId(1) });
    assert_eq!(r.phase(), Some(JobPhase::Cancelled));
    r.settle();
    assert_eq!(r.state("a"), DirState::Complete);
    assert_eq!(r.state("b"), DirState::Cancelled);
    assert!(r.m.tree.dir(r.at("b")).unwrap().reported);
    assert_eq!(r.phase(), Some(JobPhase::Cancelled));
    assert!(!r.a.has_work());
}
