use std::path::PathBuf;
use std::sync::atomic::Ordering::SeqCst;

use super::*;
use crate::shared::test_util::{Local, queued, wakes};
use crate::shared::{JobOptions, Limits};

/// Directory `n` below `/r`, as a resume would have loaded it.
fn dir(n: u64) -> VerifyDir {
    VerifyDir {
        path: PathBuf::from(format!("/r/d{n}")),
        dev: 1,
        ino: n,
        mtime: Some(0),
    }
}

#[test]
fn an_empty_batch_queues_nothing() {
    let sh = Shared::new(Local::default(), Limits::default());
    let woken = wakes(&sh);
    sh.verify(JobId(1), 0, PathBuf::from("/r"), Vec::new());
    assert_eq!(sh.outstanding.load(SeqCst), 0);
    assert_eq!(sh.submitted_seq.load(SeqCst), 0);
    assert!(sh.job(JobId(1)).is_none(), "no job made for nothing to do");
    assert!(queued(&sh.injector).is_empty());
    assert_eq!(woken.load(SeqCst), 0);
}

/// A batch is one task at the resume's root, in the ordinary queue, carrying its directories.
#[test]
fn a_batch_is_one_task_at_the_root_with_its_dirs() {
    let sh = Shared::new(Local::default(), Limits::default());
    let woken = wakes(&sh);
    sh.verify(JobId(2), 7, PathBuf::from("/r"), vec![dir(1), dir(2)]);
    assert_eq!(sh.outstanding.load(SeqCst), 1);
    assert_eq!(sh.submitted_seq.load(SeqCst), 1);
    assert_eq!(woken.load(SeqCst), 1);
    assert!(queued(&sh.priority).is_empty());
    let tasks = queued(&sh.injector);
    let [t] = &tasks[..] else {
        panic!("{} tasks", tasks.len())
    };
    assert_eq!((t.job, t.dir), (JobId(2), DirId(7)));
    assert_eq!(t.loc, PathBuf::from("/r"));
    assert!(!t.is_root && !t.probe_subvol);
    assert!(t.gate.is_none() && t.class.is_none());
    let TaskKind::Verify(v) = &t.kind else {
        panic!("not a verify task")
    };
    assert_eq!((v.batch, v.at), (7, 0));
    let paths: Vec<_> = v.dirs.iter().map(|d| d.path.clone()).collect();
    assert_eq!(paths, [PathBuf::from("/r/d1"), PathBuf::from("/r/d2")]);
    assert!(v.changed.is_empty() && v.gone.is_empty());
}

/// The batches of one resume share its job, made with default options when the resume has no
/// scan of its own, and the job of one that has keeps its options.
#[test]
fn batches_share_the_job_of_the_resume() {
    let sh = Shared::new(Local::default(), Limits::default());
    sh.verify(JobId(3), 0, PathBuf::from("/r"), vec![dir(1)]);
    let first = sh.job(JobId(3)).expect("a job");
    assert_eq!(first.opts.enter, Vec::<(u64, u64)>::new());
    sh.verify(JobId(3), 1, PathBuf::from("/r"), vec![dir(2)]);
    assert!(Arc::ptr_eq(&first, &sh.job(JobId(3)).unwrap()));
    assert_eq!(sh.outstanding.load(SeqCst), 2);

    let tmp = tempfile::tempdir().unwrap();
    let opts = JobOptions {
        enter: vec![(1, 2)],
        ..JobOptions::default()
    };
    sh.start_job(JobId(4), tmp.path(), opts);
    sh.verify(JobId(4), 0, tmp.path().to_path_buf(), vec![dir(3)]);
    assert_eq!(sh.job(JobId(4)).unwrap().opts.enter, [(1, 2)]);
}
