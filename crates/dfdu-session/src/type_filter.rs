//! The file type filter: shows only files of chosen types (video, audio, image...) or
//! extensions, everywhere below the current directory. While it is on, a snapshot counts only
//! the matching files ([`crate::snapshot::build_view`]), and a front end that shows the
//! directory's files by type and extension asks for that breakdown with it.

use crate::state::SessionState;

/// Whether the snapshot was read with another filter, or lacks the breakdown the front end
/// shows (`breakdown`).
pub fn stale(s: &SessionState, breakdown: bool) -> bool {
    s.snap.types != s.types || (breakdown && s.snap.breakdown.is_none())
}
