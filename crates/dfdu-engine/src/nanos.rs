//! Durations as whole nanoseconds, the unit of the engine's timers and timestamps.

use std::time::Duration;

/// `d` in whole nanoseconds, saturating at `u64::MAX` (584 years).
pub(crate) fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturates_past_u64() {
        assert_eq!(nanos(Duration::from_micros(3)), 3000);
        assert_eq!(nanos(Duration::MAX), u64::MAX);
    }
}
