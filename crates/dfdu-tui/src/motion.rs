//! Motion: bars that ease to new lengths, rank markers after automatic re-sorts, and deleted
//! rows kept in place as placeholders. Frames are scheduled only while something moves.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::text::Span;

use crate::app::App;
use crate::snapshot::{EntryData, Snapshot};

#[derive(Clone, Debug, Default)]
pub struct Motion {}

impl Motion {
    /// Called with each new snapshot `new` of the directory shown in `old`, before it replaces
    /// `old`: may add ghost entries for rows that were deleted.
    pub fn carry_ghosts(&mut self, _old: &Snapshot, _new: &mut Snapshot, _now: Instant) {}

    /// Called after a re-sort within one directory (`auto` for the automatic one) with the
    /// rows before and after, as indices into `entries`.
    pub fn resorted(
        &mut self,
        _old_rows: &[usize],
        _new_rows: &[usize],
        _entries: &[EntryData],
        _auto: bool,
        _now: Instant,
    ) {
    }

    /// When the next animation frame is due; `None` while nothing moves.
    pub fn next_frame_at(&self) -> Option<Instant> {
        None
    }
}

/// Turns the animations on. [`App::new`] starts with them off, so frames drawn in tests do not
/// depend on timing; the run loop calls this once before its first frame.
pub fn start(_app: &mut App) {}

/// Advances the animations of the rows on screen (`visible`: entry indices) to `now`.
pub fn frame(_app: &mut App, _now: Instant, _visible: &[usize]) {}

/// The fraction of the bar shown for `e`, whose size is `frac` of the largest row.
pub fn bar(_app: &App, _e: &EntryData, frac: f64) -> f64 {
    frac
}

/// The rank marker after the name of `e`, if it moved in the last automatic re-sort.
pub fn rank(_app: &App, _e: &EntryData) -> Option<Span<'static>> {
    None
}

/// Effects applied to the finished frame.
pub fn post_process(_app: &mut App, _buf: &mut Buffer, _now: Instant) {}
