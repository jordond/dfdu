//! When the driver reads the model, and when it wants to be called again: what it remembers
//! between ticks, and the rules on it, with no engine and no clock of its own.

use std::time::{Duration, Instant};

use dfdu_engine::SizeMode;
use dfdu_scan::{DirId, JobId};

use crate::extras::{self, Read};
use crate::state::SessionState;

/// Fastest rate scan data is read at (ncdu's default refresh is 10 per second), and how often
/// a spinner moves.
pub const DATA_FRAME: Duration = Duration::from_millis(100);

/// What the driver remembers between ticks to decide when to read the model.
#[derive(Debug)]
pub(super) struct Pacing {
    /// The directory the scanner was last asked to scan first.
    pub focus: Option<(JobId, DirId)>,
    /// The model's fingerprint at the last full read.
    pub seen_version: Option<u64>,
    /// When the front end last showed a frame.
    pub last_shown: Option<Instant>,
    /// When the snapshot was last read, how long the read held the lock, and its size mode.
    pub last_read: Option<Instant>,
    pub snap_cost: Duration,
    pub read_mode: SizeMode,
    /// How long the last read of either kind held the lock, for a debug panel.
    pub read_cost: Duration,
    /// The engine reported a change not read yet.
    pub data_dirty: bool,
    /// The first frame is shown before the scan has read anything. Until a frame has shown
    /// entries, data is read as soon as it arrives instead of waiting for `DATA_FRAME`.
    pub shown_entries: bool,
}

impl Pacing {
    pub fn new(read_mode: SizeMode) -> Self {
        Pacing {
            focus: None,
            seen_version: None,
            last_shown: None,
            last_read: None,
            snap_cost: Duration::ZERO,
            read_mode,
            read_cost: Duration::ZERO,
            data_dirty: true,
            shown_entries: false,
        }
    }

    /// Scan data is read at most every `DATA_FRAME`, and at most every nine times the cost of
    /// the last read: a read copies the current directory under the read lock, about 100 ms for
    /// one of a million entries, so the lock is held at most a tenth of the time and the scan
    /// keeps going.
    pub fn data_interval(&self) -> Duration {
        DATA_FRAME.max(self.snap_cost * 9)
    }

    /// What to read at `now`. `breakdown`: the front end shows the breakdown by file type.
    pub fn read_due(&self, s: &SessionState, breakdown: bool, now: Instant) -> Read {
        let data_ready = self.data_dirty
            && (!self.shown_entries
                || self
                    .last_read
                    .is_none_or(|t| now.duration_since(t) >= self.data_interval()));
        // Input is shown from the snapshot already held, except when it opened another
        // directory or changed what a snapshot counts.
        let stale = s.target.path != s.snap.dir_path
            || s.size_mode != self.read_mode
            || crate::type_filter::stale(s, breakdown)
            || crate::tree::stale(s.tree_shown(), &s.snap);
        extras::plan(data_ready || stale, extras::stale(s))
    }

    /// A spinner on screen is due to move at `now`.
    pub fn spinner_due(&self, s: &SessionState, now: Instant) -> bool {
        spinning(s)
            && self
                .last_shown
                .is_none_or(|t| now.duration_since(t) >= DATA_FRAME)
    }

    /// When to be called again after a tick at `now`, `None` when nothing is due until the
    /// engine changes or the user acts. `shown`: the front end shows a frame at `now`.
    /// `polling`: the history, the cache or the lookups outside the scan have more to take in.
    pub fn wake_at(
        &self,
        s: &SessionState,
        shown: bool,
        polling: bool,
        now: Instant,
    ) -> Option<Instant> {
        let since = |t: Option<Instant>| t.map_or(Duration::ZERO, |t| now.duration_since(t));
        let mut wake: Option<Duration> = None;
        let mut within = |d: Duration| wake = Some(wake.map_or(d, |w| w.min(d)));
        if self.data_dirty {
            within(self.data_interval().saturating_sub(since(self.last_read)));
        }
        if spinning(s) {
            let last = if shown { Some(now) } else { self.last_shown };
            within(DATA_FRAME.saturating_sub(since(last)));
        }
        if let Some(t) = s.next_sort_at() {
            within(t.saturating_duration_since(now));
        }
        if polling || s.outside.pending() {
            within(DATA_FRAME);
        }
        if extras::stale(s) {
            // Something after the last read changed what the extras are read for (such as a
            // re-sort of the snapshot held that moved the selection): read them again at once.
            within(Duration::ZERO);
        }
        wake.map(|d| now + d)
    }

    /// Notes a frame shown at `now`.
    pub fn shown(&mut self, s: &SessionState, now: Instant) {
        self.last_shown = Some(now);
        self.shown_entries |= !s.snap.entries.is_empty();
    }
}

/// A spinner and a clock move while the engine is busy, the cached scan is read or brought up
/// to date, or duplicates are compared; nothing animates otherwise.
pub fn spinning(s: &SessionState) -> bool {
    s.snap.busy || s.cached.is_some() || crate::dupes::running(s)
}
