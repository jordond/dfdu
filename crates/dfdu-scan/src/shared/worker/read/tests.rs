use std::fs;
use std::path::Path;

use super::*;
use crate::proto::{DirId, JobId};
use crate::shared::test_util::{Inbox, Local, Ticks, close_of, run};
use crate::shared::{JobOptions, Limits};

/// The chunks of directory `dir`, in the order sent.
fn chunks(msgs: &[ScanMsg], dir: DirId) -> Vec<&crate::proto::Chunk> {
    msgs.iter()
        .filter_map(|m| match m {
            ScanMsg::Chunk(c) if c.dir == dir => Some(c),
            _ => None,
        })
        .collect()
}

/// A scan of `root` with `limits`, run to the end on one worker.
fn scan(root: &Path, limits: Limits) -> (Shared<Local>, Vec<ScanMsg>) {
    let sh = Shared::new(Local::default(), limits);
    sh.start_job(JobId(1), root, JobOptions::default());
    let mut w = Worker::new(&sh, true);
    let msgs = run(&sh, &mut w, &Inbox::default());
    (sh, msgs)
}

#[test]
fn a_root_sends_its_stat_and_probe_before_its_entries() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), b"data").unwrap();
    let (_, msgs) = scan(dir.path(), Limits::default());
    let root = chunks(&msgs, DirId::ROOT);
    let [first, entries] = &root[..] else {
        panic!("{root:#?}");
    };
    assert_eq!(first.seq, 0);
    assert!(first.root_meta.is_some() && first.probe.is_some());
    assert!(first.files.is_empty());
    assert_eq!((entries.seq, entries.files.len()), (1, 1));
    let ScanMsg::Close { chunks, status, .. } = close_of(&msgs, DirId::ROOT) else {
        unreachable!()
    };
    assert_eq!((*chunks, status), (2, &ListingStatus::Complete));
}

#[test]
fn a_root_that_cannot_be_opened_fails_its_listing() {
    let dir = tempfile::tempdir().unwrap();
    let (sh, msgs) = scan(&dir.path().join("missing"), Limits::default());
    assert!(chunks(&msgs, DirId::ROOT).is_empty());
    let close = close_of(&msgs, DirId::ROOT);
    assert!(
        matches!(
            close,
            ScanMsg::Close {
                chunks: 0,
                status: ListingStatus::Failed(_),
                ..
            }
        ),
        "{close:?}"
    );
    assert_eq!(sh.outstanding(), (0, 0));
}

#[test]
fn blocks_go_to_chunk_tasks_while_credit_lasts() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
    }
    let limits = Limits {
        block_entries: 2,
        chunk_credit: 1,
        ..Limits::default()
    };
    let (sh, msgs) = scan(dir.path(), limits);
    assert!(sh.chunk_tasks.load(SeqCst) >= 1);
    let root = chunks(&msgs, DirId::ROOT);
    let files: usize = root.iter().map(|c| c.files.len()).sum();
    assert_eq!(files, 5);
    let ScanMsg::Close { chunks, status, .. } = close_of(&msgs, DirId::ROOT) else {
        unreachable!()
    };
    assert_eq!(*chunks as usize, root.len());
    assert_eq!(status, &ListingStatus::Complete);
    assert_eq!(sh.outstanding(), (0, 0));
}

#[test]
fn a_listing_stopped_before_its_first_block_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f"), b"x").unwrap();
    let sh = Shared::new(Local::default(), Limits::default());
    sh.start_job(JobId(1), dir.path(), JobOptions::default());
    let mut w = Worker::new(&sh, true);
    let inbox = Inbox::default();
    let clock = Ticks::default();
    // Take the task, then open the directory, which queues the root's own chunk.
    while w.slot.outbox.lock().messages().is_empty() {
        assert_eq!(w.step(&sh, &inbox, &clock), StepResult::Progress);
    }
    sh.stop_dirs(JobId(1), [DirId::ROOT]);
    let msgs = run(&sh, &mut w, &inbox);
    assert!(
        chunks(&msgs, DirId::ROOT)
            .iter()
            .all(|c| c.files.is_empty())
    );
    let close = close_of(&msgs, DirId::ROOT);
    assert!(
        matches!(
            close,
            ScanMsg::Close {
                chunks: 1,
                status: ListingStatus::Cancelled,
                ..
            }
        ),
        "{close:?}"
    );
}
