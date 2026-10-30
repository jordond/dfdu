use std::sync::atomic::Ordering::SeqCst;

use super::*;
use crate::shared::test_util::{Local, queued, wakes};
use crate::shared::{LISTING_PART, Limits};

fn shared() -> Shared<Local> {
    Shared::new(Local::default(), Limits::default())
}

/// A job starts as one root task in the priority queue, counted as submitted and outstanding.
#[test]
fn a_job_starts_with_its_root_task_first() {
    let sh = shared();
    let woken = wakes(&sh);
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(sh.start_job(JobId(1), tmp.path(), JobOptions::default()), 1);
    assert_eq!(sh.start_job(JobId(2), tmp.path(), JobOptions::default()), 2);
    assert_eq!(sh.outstanding.load(SeqCst), 2);
    assert_eq!(woken.load(SeqCst), 2);
    assert!(queued(&sh.injector).is_empty());
    let tasks = queued(&sh.priority);
    assert_eq!(tasks.len(), 2);
    let t = &tasks[0];
    assert_eq!((t.job, t.dir), (JobId(1), DirId::ROOT));
    assert_eq!(t.loc, sh.backend.loc_from_path(tmp.path()));
    assert!(t.is_root && t.probe_subvol);
    assert!(t.verify.is_none() && t.chunk.is_none() && t.class.is_none());
    assert!(sh.job(JobId(1)).is_some() && sh.job(JobId(2)).is_some());
}

/// Ending a job forgets it and tells the tasks still holding it; an unknown job is ignored.
#[test]
fn an_ended_job_is_forgotten_and_marked_ended() {
    let sh = shared();
    let tmp = tempfile::tempdir().unwrap();
    sh.start_job(JobId(1), tmp.path(), JobOptions::default());
    let held = sh.job(JobId(1)).unwrap();
    assert!(!held.ended.load(SeqCst));
    sh.end_job(JobId(1));
    assert!(held.ended.load(SeqCst));
    assert!(sh.job(JobId(1)).is_none());
    sh.end_job(JobId(9));
}

#[test]
fn stopped_dirs_are_those_named() {
    let sh = shared();
    let tmp = tempfile::tempdir().unwrap();
    sh.start_job(JobId(1), tmp.path(), JobOptions::default());
    sh.stop_dirs(JobId(1), [DirId(3), DirId(70)]);
    let j = sh.job(JobId(1)).unwrap();
    assert!(j.stopped(DirId(3)) && j.stopped(DirId(70)));
    assert!(!j.stopped(DirId(4)) && !j.stopped(DirId::ROOT));
    // No job, nothing to stop.
    sh.stop_dirs(JobId(2), [DirId(3)]);
}

/// Abandoning a part a worker runs counts it once; a worker whose outbox is held cannot be
/// checked, so the abandon is to be retried; parts no worker runs need nothing.
#[test]
fn abandoning_counts_each_running_part_once() {
    let sh = shared();
    let tmp = tempfile::tempdir().unwrap();
    sh.start_job(JobId(1), tmp.path(), JobOptions::default());
    assert!(sh.abandon(JobId(1), DirId(5)), "no worker runs it");
    assert!(sh.job(JobId(1)).unwrap().stopped(DirId(5)));
    assert_eq!(sh.outstanding_abandoned.load(SeqCst), 0);

    let slot = sh.new_slot();
    slot.current.store(pack(JobId(1), DirId(6)), SeqCst);
    slot.part.store(LISTING_PART + 2, SeqCst);
    assert!(sh.abandon(JobId(1), DirId(6)));
    assert!(sh.abandon(JobId(1), DirId(6)));
    assert_eq!(sh.outstanding_abandoned.load(SeqCst), 1);
    assert!(
        sh.abandoned
            .lock()
            .contains(&(JobId(1), DirId(6), LISTING_PART + 2))
    );

    let busy = slot.outbox.lock();
    assert!(
        !sh.abandon(JobId(1), DirId(6)),
        "the worker could not be checked"
    );
    assert!(sh.abandon(JobId(1), DirId(7)), "it runs another part");
    drop(busy);
}

#[test]
fn the_focus_is_stored_before_its_epoch_moves() {
    let sh = shared();
    let epoch = sh.focus_epoch.load(SeqCst);
    sh.set_focus(JobId(2), DirId(9));
    assert_eq!(sh.focus.load(SeqCst), pack(JobId(2), DirId(9)));
    assert_eq!(sh.focus_epoch.load(SeqCst), epoch + 1);
}
