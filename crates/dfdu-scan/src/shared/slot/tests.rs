use std::io;

use super::*;
use crate::shared::pack;

fn quiescent() -> ScanMsg {
    ScanMsg::Quiescent { submitted_seq: 1 }
}

#[test]
fn an_outbox_keeps_the_time_of_its_oldest_message() {
    let mut ob = Outbox::<()>::default();
    assert!(ob.is_empty());
    ob.push(quiescent(), 5);
    ob.push(quiescent(), 9);
    assert_eq!((ob.oldest, ob.messages().len()), (5, 2));
    assert_eq!(ob.bytes, 2 * quiescent().size());
    ob.msgs.clear();
    ob.done.push((JobId(1), DirId(2), LISTING_PART));
    assert!(!ob.is_empty(), "a finished part is still to be reported");
}

#[test]
fn a_call_not_ready_stays_open_with_its_first_start_time() {
    let slot = WorkerSlot::<()>::new();
    let since = || slot.syscall_since.load(SeqCst);
    let not_ready = slot.watch_io(10, || Err::<(), _>(io::ErrorKind::WouldBlock.into()));
    assert!(not_ready.is_err());
    assert_eq!(since(), 10);
    // The retry runs under the first start time, and its answer ends the call.
    assert_eq!(slot.watch_io(20, || Ok(since())).ok(), Some(10));
    assert_eq!(since(), 0);
    // Any other error ends it too.
    assert!(
        slot.watch_io(30, || Err::<(), _>(io::Error::other("x")))
            .is_err()
    );
    assert_eq!(since(), 0);
    assert_eq!(slot.watch(40, since), 40);
    assert_eq!(since(), 0);
}

#[test]
fn the_current_part_is_read_from_the_slot() {
    let slot = WorkerSlot::<()>::new();
    assert_eq!(slot.current(), None);
    slot.current.store(pack(JobId(3), DirId(4)), SeqCst);
    slot.part.store(2, SeqCst);
    assert_eq!(slot.current(), Some((JobId(3), DirId(4), 2)));
}
