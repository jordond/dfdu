//! Directory records and their child iterator.

use fixedbitset::FixedBitSet;

use super::Ix;
use crate::pages::Kids;
use crate::{DirId, JobId, Totals};

/// Scan state of a directory. Everything but `Pending` is terminal.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DirState {
    Pending,
    Complete,
    Partial,
    Failed,
    Skipped,
    Cancelled,
    Abandoned,
    Lost,
    Superseded,
    Deleting,
    Deleted,
}

impl DirState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        self != DirState::Pending
    }
}

/// Per-directory record. The scan bookkeeping fields are owned by the applier.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent scan bookkeeping flags, each set at its own point by the applier"
)]
pub struct DirRec {
    pub(crate) kids: Kids,
    pub(crate) total: Totals,
    pub state: DirState,
    /// State taken when `pending` reaches zero, set when the listing closes.
    pub outcome: DirState,
    pub job: JobId,
    pub dir_id: DirId,
    pub pending: u32,
    pub listing_closed: bool,
    pub reported: bool,
    pub cut: bool,
    pub stalled: bool,
    pub chunks_seen: u32,
    /// Chunk sequence numbers applied, for dropping duplicates: 0-63 in `seen_low`, the rest
    /// in `seen_high`, which stays unallocated for all but wide directories (a directory
    /// takes one chunk per 4,096 entries). Freed when the listing closes ([`DirRec::close`]),
    /// after which no chunk is applied.
    seen_low: u64,
    seen_high: FixedBitSet,
    pub changed_at: u64,
}

impl DirRec {
    pub(super) fn new() -> Self {
        DirRec {
            kids: Kids::default(),
            total: Totals::default(),
            state: DirState::Pending,
            outcome: DirState::Complete,
            job: JobId(0),
            dir_id: DirId::ROOT,
            pending: 1,
            listing_closed: false,
            reported: false,
            cut: false,
            stalled: false,
            chunks_seen: 0,
            seen_low: 0,
            seen_high: FixedBitSet::new(),
            changed_at: 0,
        }
    }

    /// Records chunk `seq` as seen. Returns false if it was already seen.
    pub fn mark_seq(&mut self, seq: u32) -> bool {
        if seq < 64 {
            let bit = 1 << seq;
            let new = self.seen_low & bit == 0;
            self.seen_low |= bit;
            return new;
        }
        let i = (seq - 64) as usize;
        self.seen_high.grow(i + 1);
        !self.seen_high.put(i)
    }

    /// Closes the listing. No more chunks are applied, so the duplicate filter is freed and
    /// the slack a directory of several chunks left in its child list is returned.
    pub fn close(&mut self, outcome: DirState) {
        self.listing_closed = true;
        self.outcome = outcome;
        self.seen_high = FixedBitSet::new();
        self.kids.shrink();
    }
}

/// Iterator over a directory's children ([`Tree::children`](super::Tree::children)).
pub struct Children<'a> {
    pub(super) head: std::slice::Iter<'a, u32>,
    pub(super) more: std::slice::Iter<'a, Vec<u32>>,
    pub(super) left: usize,
}

impl Iterator for Children<'_> {
    type Item = Ix;

    fn next(&mut self) -> Option<Ix> {
        loop {
            if let Some(&c) = self.head.next() {
                self.left -= 1;
                return Some(Ix(c));
            }
            self.head = self.more.next()?.iter();
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

impl ExactSizeIterator for Children<'_> {}

#[cfg(test)]
mod tests;
