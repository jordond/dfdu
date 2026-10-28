//! The wall clock in the unit the tree stores times in.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, as [`crate::Meta::mtime`] counts them; 0 if the clock is set
/// before it.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
