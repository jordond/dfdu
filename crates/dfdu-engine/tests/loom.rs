//! `loom` test of the applier's wake to readers (`docs/data-pipeline.md` §3 rule 2, §10.3).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" CARGO_TARGET_DIR=target/loom cargo test --release -p dfdu-engine --test loom
//! ```
//!
//! The wake is a `crossbeam-channel` `bounded(1)` channel. The test uses that channel itself,
//! with `try_send` and `try_recv` only: neither blocks, and `loom` switches threads only at its
//! own operations, so each channel call is one atomic step in every explored order. The model
//! lock is `loom`'s, and the two sides do what `applier_loop` and the TUI loop do.
#![cfg(loom)]

use crossbeam_channel::{TryRecvError, bounded};
use loom::sync::{Arc, RwLock};
use loom::thread;

/// The applier changes the model under the write lock, releases it, then `try_send`s on the
/// capacity-one channel, dropping the wake when one is already pending. The reader takes a
/// wake, then reads the model. Wakes coalesce but are never lost: once the reader has taken
/// every wake, it has read the last change, whatever the order.
#[test]
fn applier_wake_is_not_lost() {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(4);
    }
    b.check(|| {
        const LAST: u32 = 3;
        let model = Arc::new(RwLock::new(0u32));
        let (tx, rx) = bounded::<()>(1);
        let m = model.clone();
        let applier = thread::spawn(move || {
            for v in 1..=LAST {
                let mut g = m.write().unwrap();
                *g = v;
                drop(g);
                let _ = tx.try_send(());
            }
        });
        let mut seen = 0;
        loop {
            match rx.try_recv() {
                Ok(()) => seen = *model.read().unwrap(),
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => break,
            }
        }
        applier.join().unwrap();
        assert_eq!(seen, LAST, "the last change was not read after its wake");
    });
}
