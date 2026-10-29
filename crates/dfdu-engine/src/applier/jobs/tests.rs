use dfdu_scan::{DirId, JobId, JobOptions, ListingStatus, ScanMsg};
use dfdu_tree::{DirState, ErrKind};

use super::super::test_util::{Listing, Rig, chunk, close};
use super::super::{Command, JobPhase};

#[test]
fn a_cut_directory_reports_to_its_parent_at_once() {
    let mut r = Rig::two_pending();
    let a = r.at("a");
    r.a.cut(&mut r.m, a, DirState::Cancelled);
    assert_eq!(r.state("a"), DirState::Cancelled);
    assert!(r.m.tree.dir(a).unwrap().cut);
    // What the scanner still sends for it is dropped.
    r.apply(chunk(1, 0, 3, &Listing::default()));
    assert_eq!(r.m.status.stats.discarded_cut, 1);
    r.a.cut(&mut r.m, a, DirState::Abandoned);
    assert_eq!(
        r.state("a"),
        DirState::Cancelled,
        "a second cut does nothing"
    );
    r.apply(close(2, 0));
    assert_eq!(r.state(""), DirState::Complete);
    assert_eq!(r.phase(), Some(JobPhase::Finishing));
}

#[test]
fn stalled_directories_are_abandoned_after_the_limit() {
    let mut r = Rig::two_pending();
    let stalled = ScanMsg::Stalled {
        job: JobId(1),
        dir: DirId(1),
    };
    r.a.apply(&mut r.m, stalled, 100);
    r.a.tick(&mut r.m, 10_000, None);
    r.a.tick(&mut r.m, 150, Some(100));
    assert_eq!(r.state("a"), DirState::Pending);
    assert_eq!(r.m.status.stalled.len(), 1);
    r.a.tick(&mut r.m, 300, Some(100));
    assert_eq!(r.state("a"), DirState::Abandoned);
    assert_eq!(r.m.tree.error(r.at("a")), Some(ErrKind::Stalled));
    assert!(r.m.status.stalled.is_empty());
    // A close from the stuck worker that comes back later is late, not a duplicate.
    r.apply(close(1, 0));
    assert_eq!(r.m.status.stats.late, 1);
    assert_eq!(r.m.status.stats.duplicate, 0);
}

#[test]
fn only_listings_still_open_can_be_abandoned() {
    let mut r = Rig::two_pending();
    let root = r.m.tree.node_ref(r.at(""));
    r.command(Command::Abandon { node: root });
    assert_eq!(r.m.tree.error(r.at("")), None);
    let a = r.m.tree.node_ref(r.at("a"));
    r.command(Command::Abandon { node: a });
    assert_eq!(r.state("a"), DirState::Abandoned);
}

#[test]
fn finished_jobs_past_the_last_64_are_retired() {
    let mut r = Rig::scan();
    for id in 2..=70 {
        let scan = Command::Scan {
            path: format!("/r{id}").into(),
            opts: JobOptions::default(),
        };
        r.command(scan);
    }
    for id in 1..=70 {
        r.apply(ScanMsg::Close {
            job: JobId(id),
            dir: DirId::ROOT,
            chunks: 0,
            status: ListingStatus::Complete,
            classes: Vec::new(),
        });
        r.settle();
    }
    assert_eq!(r.a.jobs.len(), 64);
    assert_eq!(r.a.retired.len(), 6);
    assert!(r.a.retired.contains_key(&1) && !r.a.jobs.contains_key(&1));
    assert!(r.m.status.jobs.iter().all(|j| j.phase == JobPhase::Done));
}
