//! Text for numbers: sizes ([`human`]), compact counts, fractions and percentages, and the
//! ending of a noun counted ([`plural`]).

pub use dfdu_engine::human;

/// The ending of a noun counted `n`: `s`, or nothing for one. Irregular plurals (`entries`,
/// `matches`, `them`) are written where they are used.
#[must_use]
pub fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `part` as a fraction of `whole`, for bars and percentages.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a fraction drawn as a bar or a percentage needs a few significant digits, not 64 bits"
)]
pub fn ratio(part: u64, whole: u64) -> f64 {
    part as f64 / whole as f64
}

/// `part` as a percentage of `whole`.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a percentage is shown with one decimal, which needs a few significant digits"
)]
pub fn percent(part: u64, whole: u64) -> f64 {
    part as f64 * 100.0 / whole as f64
}

/// Compact count: 812, 1.2k, 3.4M.
#[must_use]
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", ratio(n, 1_000)),
        1_000_000..1_000_000_000 => format!("{:.1}M", ratio(n, 1_000_000)),
        _ => format!("{:.1}G", ratio(n, 1_000_000_000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(count(812), "812");
        assert_eq!(count(1_200), "1.2k");
        assert_eq!(count(3_400_000), "3.4M");
        assert_eq!(count(5_000_000_000), "5.0G");
    }

    #[test]
    fn ratios() {
        assert!((ratio(1, 4) - 0.25).abs() < f64::EPSILON);
        assert!((percent(1, 8) - 12.5).abs() < f64::EPSILON);
    }
}
