//! The values a fixture writes as text: sizes, durations, ages and times.

use std::time::Duration;

use anyhow::{Context as _, bail};
use dfdu_tree::{civil_from_days, days_from_civil};

use crate::Error;

/// Parses a size: a number with an optional fraction and unit (`B`, `KB`/`MB`/`GB`/`TB` in
/// powers of 1000, `KiB`/`MiB`/`GiB`/`TiB` in powers of 1024), or an error for what is not one.
pub(crate) fn parse_size(s: &str) -> anyhow::Result<u64> {
    let t = s.trim().replace('_', "");
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let mult: u64 = match unit.trim() {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        u => bail!("size {s:?}: unknown unit {u:?}"),
    };
    let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
    let whole: u64 = whole.parse().with_context(|| format!("size {s:?}"))?;
    let frac_val: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse().with_context(|| format!("size {s:?}"))?
    };
    let den = 10u128.pow(u32::try_from(frac.len())?);
    let part = u128::from(frac_val) * u128::from(mult) / den;
    whole
        .checked_mul(mult)
        .and_then(|b| b.checked_add(u64::try_from(part).ok()?))
        .with_context(|| format!("size {s:?} is too large"))
}

/// Parses a duration: a whole number and `ms`, `s`, `m`, `h`, `d`, `w` or `y` (365 days), or
/// an error for what is not one.
pub(crate) fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let n: u64 = t[..split]
        .parse()
        .with_context(|| format!("duration {s:?}"))?;
    let secs = match &t[split..] {
        "ms" => return Ok(Duration::from_millis(n)),
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        u => bail!("duration {s:?}: unknown unit {u:?}"),
    };
    let secs = n
        .checked_mul(secs)
        .with_context(|| format!("duration {s:?} is too long"))?;
    Ok(Duration::from_secs(secs))
}

/// Parses an age: a duration as fixtures write one (a whole number and `ms`, `s`, `m`, `h`,
/// `d`, `w` or `y`), in whole seconds.
///
/// # Errors
///
/// Not a duration.
pub fn parse_age(s: &str) -> Result<i64, Error> {
    let secs = parse_duration(s)?.as_secs();
    Ok(i64::try_from(secs).with_context(|| format!("duration {s:?} is too long"))?)
}

/// Parses `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD HH:MM:SS`, UTC, with an optional
/// trailing `Z`, into seconds since the epoch, or an error for what is not such a time.
pub(crate) fn parse_time(s: &str) -> anyhow::Result<i64> {
    let text = s.trim().trim_end_matches('Z');
    let (date, clock) = text.split_once(['T', ' ']).unwrap_or((text, "00:00:00"));
    let num = |p: &str| p.parse::<i64>().with_context(|| format!("time {s:?}"));
    let date: Vec<&str> = date.split('-').collect();
    let clock: Vec<&str> = clock.split(':').collect();
    let ([year, month, day], [hour, minute, second]) = (date.as_slice(), clock.as_slice()) else {
        bail!("time {s:?}: expected YYYY-MM-DD[THH:MM:SS]");
    };
    let (year, month, day) = (num(year)?, num(month)?, num(day)?);
    let (hour, minute, second) = (num(hour)?, num(minute)?, num(second)?);
    // A date that does not exist (February 31st) comes back as another one.
    let days = days_from_civil(year, month, day);
    if !(1..=12).contains(&month) || civil_from_days(days) != (year, month, day) {
        bail!("time {s:?}: no such date");
    }
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        bail!("time {s:?}: no such time of day");
    }
    Ok(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// `secs` as `YYYY-MM-DD HH:MM:SS` UTC, as libfaketime's `FAKETIME` takes it after an `@`.
#[must_use]
pub fn format_time(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_take_fractions_units_and_separators() {
        assert_eq!(parse_size("1.5KiB").unwrap(), 1536);
        assert_eq!(parse_size("2GB").unwrap(), 2_000_000_000);
        assert_eq!(parse_size("1_000").unwrap(), 1000);
        assert_eq!(parse_size(" 512 B ").unwrap(), 512);
        assert_eq!(parse_size("0.25MiB").unwrap(), 1 << 18);
        assert_eq!(parse_size("1TiB").unwrap(), 1 << 40);
        assert!(parse_size("3 parsecs").is_err());
        assert!(parse_size("GB").is_err());
        assert!(parse_size("1.2.3MB").is_err());
        // Overflow is an error rather than a wrong value.
        assert!(parse_size("20000000000000000000TiB").is_err());
    }

    #[test]
    fn durations_and_ages_take_their_units() {
        assert_eq!(parse_duration("300ms").unwrap(), Duration::from_millis(300));
        assert_eq!(parse_duration("2w").unwrap().as_secs(), 14 * 86_400);
        assert_eq!(parse_duration("1y").unwrap().as_secs(), 365 * 86_400);
        assert_eq!(parse_age("90m").unwrap(), 5400);
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("3 fortnights").is_err());
        assert!(parse_duration("99999999999999999y").is_err());
        assert!(parse_age("18446744073709551615s").is_err());
    }

    #[test]
    fn times_parse_in_each_form_and_format_back() {
        assert_eq!(parse_time("1970-01-01").unwrap(), 0);
        assert_eq!(parse_time("1970-01-02").unwrap(), 86_400);
        let t = parse_time("2026-10-01T09:00:00Z").unwrap();
        assert_eq!(format_time(t), "2026-10-01 09:00:00");
        assert_eq!(parse_time("2026-10-01 09:00:00").unwrap(), t);
        assert_eq!(parse_time("2026-10-01T09:00:00").unwrap(), t);
        assert_eq!(format_time(-1), "1969-12-31 23:59:59");
        assert!(parse_time("2024-02-29").is_ok());
    }

    #[test]
    fn times_that_do_not_exist_are_refused() {
        for bad in [
            "2026-02-31",
            "2025-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-02-28T25:00:00",
            "2026-02-28T12:60:00",
            "2026-02-28T12:00",
            "yesterday",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
    }
}
