//! The wall clock in the unit the tree stores times in, days counted in that unit as calendar
//! dates, and those times written out as UTC dates.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, as [`crate::Meta::mtime`] counts them; 0 if the clock is set
/// before it.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Days since 1970-01-01 of a proleptic Gregorian year, month and day (Howard Hinnant's
/// algorithm). A day past the end of its month counts on into the next.
#[must_use]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // In 400-year eras starting on 1 March.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 as a proleptic Gregorian year, month and day: the inverse of
/// [`days_from_civil`].
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Seconds since the Unix epoch as a UTC date ("2026-10-18").
#[must_use]
pub fn day(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Seconds since the Unix epoch as a UTC date and time ("2026-10-24 09:42 UTC").
#[must_use]
pub fn date_time(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip_through_days() {
        for days in [-719_468, -1, 0, 59, 365, 11_016, 20_727, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn days_past_the_end_of_a_month_run_into_the_next() {
        assert_eq!(civil_from_days(days_from_civil(2026, 2, 31)), (2026, 3, 3));
    }

    #[test]
    fn dates_and_times_are_utc() {
        assert_eq!(day(86_400 * 3), "1970-01-04");
        assert_eq!(day(1_793_157_288), "2026-10-28");
        assert_eq!(day(-1), "1969-12-31");
        assert_eq!(date_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(date_time(1_793_157_288), "2026-10-28 03:14 UTC");
        assert_eq!(date_time(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(date_time(-86_400), "1969-12-31 00:00 UTC");
        assert_eq!(date_time(-1), "1969-12-31 23:59 UTC");
    }

    #[test]
    fn the_clock_is_after_the_epoch() {
        assert!(now_secs() > 1_700_000_000);
    }
}
