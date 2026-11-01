//! When the rows are sorted again: before the next frame when asked, otherwise automatically
//! once the user has been idle.

use std::time::{Duration, Instant};

/// Automatic re-sort after this long without input...
pub const IDLE_BEFORE_SORT: Duration = Duration::from_millis(1500);
/// ...and at most this often.
pub const MIN_SORT_INTERVAL: Duration = Duration::from_secs(2);

/// When the rows are sorted again: before the next frame when asked, otherwise automatically
/// once the user has been idle for [`IDLE_BEFORE_SORT`], at most every [`MIN_SORT_INTERVAL`].
#[derive(Debug)]
pub struct SortSchedule {
    last_input: Instant,
    last_sort: Instant,
    /// An exact sort is asked for.
    requested: bool,
    /// The filter changed: its rows are needed before the next frame.
    refilter: bool,
    /// A snapshot that changed came in since the last sort.
    changed: bool,
}

impl SortSchedule {
    /// The schedule of a browser opened at `now`: idle, with an exact sort asked for.
    pub fn new(now: Instant) -> Self {
        SortSchedule {
            // No key pressed yet counts as idle.
            last_input: now.checked_sub(IDLE_BEFORE_SORT).unwrap_or(now),
            last_sort: now,
            requested: true,
            refilter: false,
            changed: false,
        }
    }

    pub fn request_sort(&mut self) {
        self.requested = true;
    }

    pub fn request_refilter(&mut self) {
        self.refilter = true;
    }

    pub fn note_input(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// A snapshot that changed came in since the last sort.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Records that a snapshot that changed came in.
    pub fn note_changed(&mut self) {
        self.changed = true;
    }

    /// When an automatic sort is due, if one is pending.
    pub fn due_at(&self) -> Instant {
        (self.last_input + IDLE_BEFORE_SORT).max(self.last_sort + MIN_SORT_INTERVAL)
    }

    fn idle(&self, now: Instant) -> bool {
        now.duration_since(self.last_input) >= IDLE_BEFORE_SORT
    }

    /// An automatic sort is due at `now`: one is `pending` and the user has been idle, long
    /// enough after the last sort or right after the scan `finished`.
    pub fn auto_due(&self, pending: bool, finished: bool, now: Instant) -> bool {
        let interval = now.duration_since(self.last_sort) >= MIN_SORT_INTERVAL;
        self.idle(now) && ((pending && interval) || finished)
    }

    /// Takes the requests: an exact sort, and the rows of a changed filter.
    pub fn take_requests(&mut self) -> (bool, bool) {
        (
            std::mem::take(&mut self.requested),
            std::mem::take(&mut self.refilter),
        )
    }

    /// Records a sort at `now`, which took in every snapshot that changed before it.
    pub fn record_sort(&mut self, now: Instant) {
        self.last_sort = now;
        self.changed = false;
    }
}
