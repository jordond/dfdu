use std::io;
use std::path::PathBuf;

use dfdu_tree::ErrKind;

use super::*;
use crate::backend::{Block, RawBlock};
use crate::shared::test_util::task;

/// A block with nothing in it to stat.
struct NoEntries;

impl RawBlock<PathBuf> for NoEntries {
    fn len(&self) -> usize {
        0
    }
    fn stat(&mut self, _out: &mut Block<PathBuf>) -> io::Result<()> {
        Ok(())
    }
}

fn chunk(seq: u32) -> TaskKind<PathBuf> {
    TaskKind::Chunk(Box::new(ChunkWork {
        raw: Box::new(NoEntries),
        seq,
        parts: Arc::new(DirParts::new()),
        class: None,
    }))
}

fn verify(batch: u32) -> TaskKind<PathBuf> {
    TaskKind::Verify(Box::new(verify::VerifyWork {
        batch,
        ..verify::VerifyWork::default()
    }))
}

/// Each kind gives up only its own work: a listing task has neither a block nor a batch.
#[test]
fn a_task_kind_hands_out_only_its_own_work() {
    let mut listing = TaskKind::<PathBuf>::Listing;
    assert!(listing.chunk().is_none() && listing.chunk_mut().is_none());
    assert!(listing.verify_mut().is_none());

    let mut c = chunk(3);
    assert_eq!(c.chunk().map(|w| w.seq), Some(3));
    c.chunk_mut().expect("a block").seq = 4;
    assert_eq!(c.chunk().map(|w| w.seq), Some(4));
    assert!(c.verify_mut().is_none());

    let mut v = verify(7);
    assert_eq!(v.verify_mut().map(|w| w.batch), Some(7));
    assert!(v.chunk().is_none() && v.chunk_mut().is_none());
}

/// Taking the work out of an ending task leaves a listing task behind.
#[test]
fn taking_the_work_leaves_a_listing_task() {
    let mut c = chunk(2);
    let taken = c.take();
    assert_eq!(taken.chunk().map(|w| w.seq), Some(2));
    assert!(matches!(c, TaskKind::Listing));
    let mut v = verify(5);
    assert!(matches!(v.take(), TaskKind::Verify(w) if w.batch == 5));
    assert!(matches!(v, TaskKind::Listing));
}

/// A task's debug form names its job, directory and location, and the block of a chunk task.
#[test]
fn a_task_shows_its_block_in_debug() {
    let mut t = task(DirId(9), PathBuf::from("/r/d"), 1, 1);
    let listing = format!("{t:?}");
    assert!(listing.contains("dir: DirId(9)"), "{listing}");
    assert!(listing.contains("\"/r/d\""), "{listing}");
    assert!(listing.contains("chunk: None"), "{listing}");
    t.kind = chunk(6);
    let chunked = format!("{t:?}");
    assert!(chunked.contains("chunk: Some(6)"), "{chunked}");
}

/// The listing task is one of the unfinished parts; only the chunk tasks are in flight.
#[test]
fn chunk_tasks_in_flight_leave_the_listing_task_out() {
    let parts = DirParts::new();
    assert_eq!(parts.in_flight(), 0);
    parts.unfinished.fetch_add(2, SeqCst);
    assert_eq!(parts.in_flight(), 2);
    // Once the listing task has finished too, every unfinished part is a chunk task.
    parts.unfinished.store(0, SeqCst);
    assert_eq!(parts.in_flight(), 0);
}

/// A directory is complete only when every part is; otherwise its status adds up what the
/// chunk tasks and the listing task each met.
#[test]
fn a_directory_adds_up_the_outcome_of_every_part() {
    let partial = |panicked, cancelled, read_error| ListingStatus::Partial {
        panicked,
        cancelled,
        read_error,
    };
    let parts = DirParts::new();
    assert_eq!(parts.status(), ListingStatus::Complete);
    *parts.listing.lock() = Some(ListingStatus::Complete);
    assert_eq!(parts.status(), ListingStatus::Complete);

    parts.panicked.store(1, SeqCst);
    parts.cancelled.store(2, SeqCst);
    assert_eq!(parts.status(), partial(1, 2, None));
    *parts.listing.lock() = Some(partial(3, 1, Some(ErrKind::NotFound)));
    assert_eq!(parts.status(), partial(4, 3, Some(ErrKind::NotFound)));
    // A listing cut short counts as one more cancelled part.
    *parts.listing.lock() = Some(ListingStatus::Cancelled);
    assert_eq!(parts.status(), partial(1, 3, None));

    let parts = DirParts::new();
    *parts.listing.lock() = Some(ListingStatus::Failed(ErrKind::PermissionDenied));
    let read_error = Some(ErrKind::PermissionDenied);
    assert_eq!(parts.status(), partial(0, 0, read_error));
}
