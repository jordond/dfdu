use std::fs;
use std::path::Path;

use super::*;
use crate::proto::JobId;
use crate::shared::test_util::{Inbox, Local, run};
use crate::shared::{Limits, VerifyDir};

/// `path` as a resume would have loaded it, its mtime moved by `mtime` (`None`: one that cannot
/// vouch for it).
fn loaded(path: &Path, mtime: Option<i64>) -> VerifyDir {
    let m = Local::default().stat_root(&path.to_path_buf()).unwrap();
    VerifyDir {
        path: path.to_path_buf(),
        dev: m.dev,
        ino: m.ino,
        mtime: mtime.map(|d| m.mtime + d),
    }
}

/// A batch reports, by index, the directories whose mtime moved or cannot vouch for them, and
/// those gone, replaced, or no longer directories.
#[test]
fn a_batch_reports_what_is_not_as_loaded() {
    let dir = tempfile::tempdir().unwrap();
    for d in ["same", "moved", "recent", "replaced"] {
        fs::create_dir(dir.path().join(d)).unwrap();
    }
    fs::write(dir.path().join("file"), b"x").unwrap();
    let at = |n: &str| dir.path().join(n);
    let mut replaced = loaded(&at("replaced"), Some(0));
    replaced.ino += 1;
    let mut gone = loaded(&at("same"), Some(0));
    gone.path = at("gone");
    let dirs = vec![
        loaded(&at("same"), Some(0)),
        loaded(&at("moved"), Some(-5)),
        loaded(&at("recent"), None),
        gone,
        replaced,
        loaded(&at("file"), Some(0)),
    ];
    let sh = Shared::new(Local::default(), Limits::default());
    sh.verify(JobId(4), 3, dir.path().to_path_buf(), dirs);
    let mut w = Worker::new(&sh, true);
    let msgs = run(&sh, &mut w, &Inbox::default());
    let [
        ScanMsg::Verified {
            job,
            batch,
            changed,
            gone,
            complete,
        },
    ] = &msgs[..]
    else {
        panic!("{msgs:#?}");
    };
    assert_eq!((job.0, *batch, *complete), (4, 3, true));
    assert_eq!((&changed[..], &gone[..]), (&[1, 2][..], &[3, 4, 5][..]));
    assert_eq!(sh.outstanding(), (0, 0));
}

/// A batch of a job that ended stats nothing and says so.
#[test]
fn a_batch_of_an_ended_job_reports_it_did_not_check() {
    let dir = tempfile::tempdir().unwrap();
    let sh = Shared::new(Local::default(), Limits::default());
    sh.verify(
        JobId(4),
        0,
        dir.path().to_path_buf(),
        vec![loaded(dir.path(), Some(-1))],
    );
    sh.end_job(JobId(4));
    let mut w = Worker::new(&sh, true);
    let msgs = run(&sh, &mut w, &Inbox::default());
    assert!(
        matches!(
            &msgs[..],
            [ScanMsg::Verified { changed, gone, complete: false, .. }]
                if changed.is_empty() && gone.is_empty()
        ),
        "{msgs:#?}"
    );
    assert_eq!(sh.outstanding(), (0, 0));
}
