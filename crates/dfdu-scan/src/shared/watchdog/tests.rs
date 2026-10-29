use std::path::Path;

use super::*;
use crate::proto::{DirId, JobId};
use crate::shared::test_util::{Inbox, Local, run};
use crate::shared::{JobOptions, Limits, Worker, pack};

fn shared() -> Shared<Local> {
    let limits = Limits {
        stall_ns: 100,
        ..Limits::default()
    };
    Shared::new(Local::default(), limits)
}

fn stalls(msgs: &[ScanMsg]) -> Vec<(JobId, DirId)> {
    msgs.iter()
        .filter_map(|m| match m {
            ScanMsg::Stalled { job, dir } => Some((*job, *dir)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_stuck_call_is_reported_once() {
    let sh = shared();
    let slot = sh.new_slot();
    slot.current.store(pack(JobId(1), DirId(7)), SeqCst);
    slot.syscall_since.store(1000, SeqCst);
    let inbox = Inbox::default();
    assert_eq!(sh.watchdog_round(&inbox, 1050).stuck, 0);
    let stuck = WatchdogReport {
        stuck: 1,
        stuck_abandoned: 0,
    };
    assert_eq!(sh.watchdog_round(&inbox, 1200), stuck);
    assert_eq!(sh.watchdog_round(&inbox, 1300), stuck);
    assert_eq!(stalls(&inbox.take()), [(JobId(1), DirId(7))]);
    // Once the applier abandons the part, it is counted apart.
    assert!(sh.abandon(JobId(1), DirId(7)));
    let rep = sh.watchdog_round(&inbox, 1400);
    assert_eq!((rep.stuck, rep.stuck_abandoned), (1, 1));
    assert!(stalls(&inbox.take()).is_empty());
}

#[test]
fn quiescent_is_sent_once_per_submission() {
    let sh = shared();
    let inbox = Inbox::default();
    sh.watchdog_round(&inbox, 1);
    sh.watchdog_round(&inbox, 2);
    let quiet = |msgs: Vec<ScanMsg>| -> Vec<u64> {
        msgs.into_iter()
            .filter_map(|m| match m {
                ScanMsg::Quiescent { submitted_seq } => Some(submitted_seq),
                _ => None,
            })
            .collect()
    };
    assert_eq!(quiet(inbox.take()), [0]);
    let seq = sh.start_job(JobId(1), Path::new("/nonexistent"), JobOptions::default());
    sh.watchdog_round(&inbox, 3);
    assert!(
        quiet(inbox.take()).is_empty(),
        "the root task is outstanding"
    );
    let mut w = Worker::new(&sh, true);
    run(&sh, &mut w, &inbox);
    sh.watchdog_round(&inbox, 4);
    sh.watchdog_round(&inbox, 5);
    assert_eq!(quiet(inbox.take()), [seq]);
}

#[test]
fn extra_workers_are_capped_for_live_and_abandoned_stalls() {
    let sh = shared();
    let rep = |stuck, stuck_abandoned| WatchdogReport {
        stuck,
        stuck_abandoned,
    };
    assert_eq!(sh.allowed_extra_workers(8, &rep(3, 0)), 3);
    assert_eq!(sh.allowed_extra_workers(8, &rep(20, 0)), 8);
    assert_eq!(sh.allowed_extra_workers(64, &rep(40, 0)), 16);
    assert_eq!(sh.allowed_extra_workers(8, &rep(100, 90)), 8 + 64);
}
