//! The applier's inbox: the one channel scanner data and control messages reach it through.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crossbeam_channel::{Receiver, Sender, unbounded};
use dfdu_scan::{Batch, Budget, ScanMsg, Sink};

/// The applier's inbox: one channel, so control messages keep their order relative to data,
/// plus the byte budget that bounds data in transit.
pub(super) struct Inbox {
    tx: Sender<Batch>,
    budget: Arc<Budget>,
    /// Messages sent so far, counted before they enter the channel: a reader that sees the
    /// applier's count of messages received under the lock sees at least as many sent.
    sent: Arc<AtomicU64>,
}

impl Inbox {
    pub(super) fn new(
        budget_bytes: usize,
    ) -> (Inbox, Receiver<Batch>, Arc<Budget>, Arc<AtomicU64>) {
        let (tx, rx) = unbounded();
        let budget = Arc::new(Budget::new(budget_bytes));
        let sent = Arc::new(AtomicU64::new(0));
        (
            Inbox {
                tx,
                budget: budget.clone(),
                sent: sent.clone(),
            },
            rx,
            budget,
            sent,
        )
    }

    fn push(&self, batch: Batch) {
        self.sent.fetch_add(batch.msgs.len() as u64, Relaxed);
        let _ = self.tx.send(batch);
    }
}

impl Sink for Inbox {
    fn try_send(&self, mut batch: Batch) -> Result<(), Batch> {
        match self.budget.try_acquire(batch.bytes) {
            Some(n) => {
                batch.bytes = n;
                self.push(batch);
                Ok(())
            }
            None => Err(batch),
        }
    }

    fn send(&self, mut batch: Batch) {
        batch.bytes = self.budget.acquire(batch.bytes);
        self.push(batch);
    }

    fn send_control(&self, msg: ScanMsg) {
        self.push(Batch {
            msgs: vec![msg],
            bytes: 0,
        });
    }
}

#[cfg(test)]
mod tests;
